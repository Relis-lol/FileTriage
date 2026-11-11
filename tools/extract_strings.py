"""Heuristically find visible text left outside the locale catalog.

The scanner examines every JavaScript string literal, including function
arguments, templates, ternaries, fallbacks, and composed messages.

This is an advisory project audit, not a complete ECMAScript parser. Its
self-test is a release gate for the documented sinks; a zero result means
"zero findings from this heuristic audit", not proof that no visible literal
exists anywhere in JavaScript.
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from dataclasses import dataclass

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
UI = os.path.join(ROOT, "ui")

TECHNICAL = {
    "afterbegin", "all", "any", "app", "archive", "arrowleft", "arrowright",
    "audio", "auto", "backspace", "beforeend", "button", "cancelled", "check",
    "checkcircle", "code", "completed", "conflict", "dark", "date", "datetime-local",
    "delete", "development", "direct", "disk_image", "done", "download", "ebook",
    "enter", "error", "failed", "false", "file", "files", "folder", "font", "home",
    "image", "info", "installer", "kept", "left", "light", "manual", "medium",
    "microsoft_store", "move", "none", "number", "old", "open", "pending", "pdf",
    "planned", "presentation", "purchased", "recovery", "rename", "right", "rolled_back",
    "rules", "safe", "search", "select", "settings", "skip", "skipped", "small", "space",
    "spreadsheet", "store", "store_unavailable", "success", "system", "temp", "text",
    "torrent", "trash", "trial_active", "trial_expired", "true", "unknown", "video",
    "warning",
    "backenderror", "deutsch", "english", "escape", "gb", "input", "mb", "tab", "tb",
    "textarea",
    # Audited UI protocol values, DOM/SVG names, event names, locale/unit
    # identifiers and internal view states. This is deliberately explicit:
    # an arbitrary lowercase word in a function argument is still reported.
    "active", "attention", "page", "refresh", "aktiv", "dragover", "drop", "change",
    "keydown", "mousedown", "click", "solo", "large", "object", "div", "html", "on",
    "function", "dataset", "btn", "toast", "pseudo", "rtl", "ltr", "byte",
    "kilobyte", "megabyte", "gigabyte", "terabyte", "unit", "short", "year", "month",
    "day", "hour", "minute", "second", "package", "music", "monitor", "transfer",
    "hourglass", "book", "disc", "link", "svg", "width", "height", "fill", "stroke",
    "round", "focusable", "icon", "rect", "string", "path", "circle", "cx", "cy", "rx",
    "permanent", "size", "modified", "document", "name", "ext", "parent", "descending",
    "ascending", "gedreht", "selected", "tr", "verschoben", "copy", "duplicates", "drive",
    "clock", "danger", "warn", "ok", "kritisch", "primary", "contains", "eq", "in", "gt",
    "lt", "category", "muster", "typ", "suggested", "pause", "play", "pencil", "user",
    "allgemein", "erweitert", "eye", "aus", "passiv", "inbox", "shield", "recycle", "lock",
    "view", "triage", "uebersprungen", "behalten", "ask", "collision", "reveal", "target",
    "ar", "he", "fa", "ur", "en", "zh", "zh-hant", "zh-hans", "pt", "br", "pt-br",
    "pt-pt", "nb", "no", "nn",
    "aria-current", "aria-label", "aria-keyshortcuts", "aria-hidden", "aria-modal",
    "aria-selected", "aria-sort", "aria-valuemin", "aria-valuemax", "aria-valuenow",
    "data-groesse", "data-treffer", "text-3", "stroke-width", "stroke-linecap",
    "stroke-linejoin",
}

@dataclass
class Literal:
    line: int
    value: str
    start: int


def regex_can_start(source: str, offset: int) -> bool:
    before = source[:offset].rstrip()
    if not before:
        return True
    if before[-1] in "(=:[,!&|?{};":
        return True
    return bool(re.search(r"\b(?:return|throw|case|yield|await)\s*$", before))


def skip_regex(source: str, offset: int) -> int:
    i = offset + 1
    in_class = False
    while i < len(source):
        if source[i] == "\\":
            i += 2
            continue
        if source[i] == "[":
            in_class = True
        elif source[i] == "]":
            in_class = False
        elif source[i] == "/" and not in_class:
            i += 1
            while i < len(source) and source[i].isalpha():
                i += 1
            return i
        i += 1
    return i


def files() -> list[str]:
    result: list[str] = []
    for directory, _, names in os.walk(UI):
        if "locales" in directory:
            continue
        result.extend(os.path.join(directory, n) for n in names if n.endswith(".js"))
    return sorted(result)


def literals(source: str, line_offset: int = 0, start_offset: int = 0) -> list[Literal]:
    """Small JS lexer that also descends into template interpolations."""
    result: list[Literal] = []
    i = 0
    line = 1 + line_offset
    while i < len(source):
        if source.startswith("//", i):
            end = source.find("\n", i)
            if end < 0:
                break
            i = end
            continue
        if source.startswith("/*", i):
            end = source.find("*/", i + 2)
            if end < 0:
                break
            line += source.count("\n", i, end + 2)
            i = end + 2
            continue
        # Regex literals may contain quotes and comment-looking text. This is
        # deliberately conservative and covers expression positions where a
        # regex can begin; division remains ordinary punctuation.
        if source[i] == "/" and regex_can_start(source, i):
            end = skip_regex(source, i)
            line += source.count("\n", i, end)
            i = end
            continue
        quote = source[i]
        if quote not in "'\"`":
            if quote == "\n":
                line += 1
            i += 1
            continue
        start = i
        start_line = line
        i += 1
        chars: list[str] = []
        while i < len(source):
            char = source[i]
            if char == "\\" and i + 1 < len(source):
                chars.extend((char, source[i + 1]))
                i += 2
                continue
            if quote == "`" and source.startswith("${", i):
                # Find the matching brace with full string/template awareness,
                # then recursively scan the JavaScript expression.
                expr_start = i + 2
                depth = 1
                j = expr_start
                expr_line = line
                while j < len(source) and depth:
                    if source[j] == "/" and regex_can_start(source[expr_start:j], j - expr_start):
                        j = skip_regex(source, j)
                        continue
                    if source[j] in "'\"`":
                        q = source[j]
                        j += 1
                        while j < len(source):
                            if source[j] == "\\":
                                j += 2
                                continue
                            if source[j] == q:
                                j += 1
                                break
                            j += 1
                        continue
                    if source.startswith("//", j):
                        end = source.find("\n", j)
                        j = len(source) if end < 0 else end
                        continue
                    if source.startswith("/*", j):
                        end = source.find("*/", j + 2)
                        j = len(source) if end < 0 else end + 2
                        continue
                    depth += (source[j] == "{") - (source[j] == "}")
                    j += 1
                expression = source[expr_start:j - 1] if depth == 0 else source[expr_start:]
                result.extend(literals(expression, expr_line - 1, start_offset + expr_start))
                chars.append("${}")
                line += source.count("\n", i, j)
                i = j
                continue
            if char == quote:
                i += 1
                break
            if char == "\n":
                line += 1
            chars.append(char)
            i += 1
        result.append(Literal(start_line, "".join(chars), start_offset + start))
    return result


def is_translation_key(source: str, literal: Literal) -> bool:
    before = source[max(0, literal.start - 40):literal.start]
    return bool(re.search(r"(?:\bt|\bplural)\s*\(\s*$", before))


def is_import(source: str, literal: Literal) -> bool:
    line_start = source.rfind("\n", 0, literal.start) + 1
    before = source[line_start:literal.start]
    return bool(re.search(r"\b(?:from|import)\s*$", before)) or literal.value.startswith("./")


def is_code_metadata(source: str, literal: Literal) -> bool:
    before = source[max(0, literal.start - 80):literal.start]
    return bool(
        re.search(r"\.className\s*=\s*$", before)
        or re.search(r"\b(?:class|icon|kind|type|role|view|field|op|id)\s*:\s*$", before)
        or re.search(r"\b(?:kind|type|role|view|field|op|id)\s*=\s*$", before)
        or re.search(r"\bclass\s*:[^,{}\n]*$", before)
        or re.search(r"\b(?:el|icon|invoke|listenTo?)\s*\(\s*$", before)
        or re.search(r"\.classList\.(?:add|remove|toggle)\s*\(\s*$", before)
        or (
            literal.value.lower() in {"info", "success", "error", "warning"}
            and re.search(r"\btoast\s*\([^\n]*,\s*$", before)
        )
    )


def is_display_context(source: str, literal: Literal) -> bool:
    before = source[max(0, literal.start - 100):literal.start]
    return bool(
        re.search(r"\b(?:toast|showToast|alert|confirm|fehlerDialog|dialog)\s*\([^()]*$", before)
        or re.search(
            r"\b(?:text|title|body|label|hint|message|placeholder|reason|note)\s*:\s*$",
            before,
        )
        or re.search(r"\.(?:textContent|innerText|alt|title)\s*=\s*$", before)
        or re.search(r"\.setAttribute\(\s*['\"](?:aria-label|title|alt)['\"]\s*,\s*$", before)
    )


def is_display_text(value: str, visible_sink: bool = False) -> bool:
    value = value.replace("\\n", " ").replace("\\t", " ").strip()
    if len(value) < 2 or not any(char.isalpha() for char in value):
        return False
    if "var(" in value or (":" in value and (";" in value or re.match(r"^[a-z-]+:", value))):
        return False
    if value.startswith((".", "[", "#", "(")):
        return False
    if re.match(r"^[a-z]+\[", value):
        return False
    if re.match(r"^[Mm]\d", value):
        return False
    if value.isupper():
        return False
    if re.fullmatch(r"[a-z]+(?:,\s*(?:\[[^]]+\]|[a-z]+))+", value):
        return False
    if not visible_sink and value.lower() in TECHNICAL:
        return False
    if re.fullmatch(r"[a-z][a-z0-9_-]*_[a-z0-9_-]+", value):
        return False
    if re.fullmatch(r"[a-z][a-z0-9_-]*(?:\.[a-z0-9_${}-]+)+", value):
        return False
    if re.fullmatch(r"[a-z][a-z0-9_-]*\.\$\{\}", value):
        return False
    if "${}" in value and "." in value and not any(char.isspace() for char in value):
        return False
    if re.fullmatch(r"[a-z]+[A-Z][A-Za-z0-9]*", value):
        return False
    if re.fullmatch(r"\.?[a-z0-9_*?-]+\.[a-z0-9*?-]+", value, re.I):
        return False
    return True


def findings(source: str) -> list[Literal]:
    return [
        item for item in literals(source)
        if not is_translation_key(source, item)
        and not is_import(source, item)
        and not is_code_metadata(source, item)
        and is_display_text(item.value, is_display_context(source, item))
    ]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--count", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--advisory", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        cases = {
            "el text": ("el('button', { class: 'danger', text: 'delete' })", {"delete"}),
            "aria label": ("node.setAttribute('aria-label', 'delete')", {"delete"}),
            "textContent": ("button.textContent = 'retry'", {"retry"}),
            "innerText": ("button.innerText = 'missing'", {"missing"}),
            "alt": ("img.alt = 'preview'", {"preview"}),
            "template": ("showToast(`Deleted ${count} files`)", {"Deleted ${} files"}),
            "ternary sink": ("showToast(condition ? 'present' : 'missing')", {"present", "missing"}),
            "fallback": ("showToast(foo ?? 'missing')", {"missing"}),
            "nested template": (
                "showToast(`${condition ? `Deleted ${count}` : 'retry'}`)",
                {"Deleted ${}", "retry"},
            ),
            "regex brace in template": (
                "showToast(`${/}/.test(x) ? 'present' : 'missing'}`)",
                {"present", "missing"},
            ),
            "regex backtick in template": (
                "showToast(`${/`[^`]+`/.test(x) ? 'present' : 'missing'}`)",
                {"present", "missing"},
            ),
            "comment and object": (
                "showToast(`${x /* } */ ? ({a:{b:'present'}}).a.b : 'missing'}`)",
                {"present", "missing"},
            ),
        }
        for name, (source, expected) in cases.items():
            actual = {item.value for item in findings(source)}
            if not expected.issubset(actual):
                print(f"self-test failed: {name}: expected {expected!r}, got {actual!r}", file=sys.stderr)
                return 2
        clean = "// 'Comment text'\nconst x = t('common.cancel'); const kind = 'error';"
        if findings(clean):
            print("self-test failed: translated/technical text was reported", file=sys.stderr)
            return 2
        regex_clean = "function f(){ return /'not visible'/.test(x) } const r = /['\"`]\\/\\/* not text/;"
        if findings(regex_clean):
            print("self-test failed: regex content was reported", file=sys.stderr)
            return 2
        protocol_clean = "function toast(message, kind = 'info') {} toast(t('x'), 'error');"
        if findings(protocol_clean):
            print("self-test failed: toast kind was reported as display text", file=sys.stderr)
            return 2
        if {item.value for item in findings("showToast('error')")} != {"error"}:
            print("self-test failed: visible technical-looking text was suppressed", file=sys.stderr)
            return 2
        print("string-audit self-test passed")
        return 0
    found: list[tuple[str, int, str]] = []
    for path in files():
        with open(path, encoding="utf-8") as handle:
            source = handle.read()
        found.extend(
            (os.path.relpath(path, ROOT), item.line, item.value)
            for item in findings(source)
        )
    if args.count:
        print(len(found))
    else:
        for path, line, value in found:
            print(f"{path}:{line}: {value[:100]}")
        print(
            f"\n{len(found)} findings from heuristic string audit "
            f"in {len(files())} files"
        )
    return 0 if args.advisory else (1 if found else 0)


if __name__ == "__main__":
    sys.exit(main())
