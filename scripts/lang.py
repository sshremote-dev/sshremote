#!/usr/bin/env python3
"""The words to translate and the state of each language's table (assets/lang/<code>.json).

  scripts/lang.py keys            every English text marked with t!() or n!(), one JSON list
  scripts/lang.py check           what each table lacks, keeps for nothing, or fills wrongly
  scripts/lang.py sync            drops words no longer in the program from every table
  scripts/lang.py todo CODE [N]   up to N words CODE lacks, tagged: {"3fa9c2d1": "English", ...}
  scripts/lang.py put CODE FILE   takes {"3fa9c2d1": "translation", ...} for those tags into CODE
  scripts/lang.py pairs CODE [FROM [N]]   N texts from FROM on, to review: {"tag": ["English", "translation"]}
"""

import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LANG = os.path.join(ROOT, "assets", "lang")
MARK = re.compile(r'(?<![A-Za-z0-9_])[tn]!\(\s*(?:"((?:[^"\\]|\\.)*)"|r(#*)"(.*?)"\2)', re.S)
SLOT = re.compile(r"\{(\d*)\}")


def unescape(s):
    out, i = [], 0
    while i < len(s):
        c = s[i]
        if c != "\\":
            out.append(c)
            i += 1
            continue
        n = s[i + 1]
        if n == "\n":
            # A line ending escaped: it and the next line's leading blanks are left out.
            i += 2
            while i < len(s) and s[i] in " \t\n\r":
                i += 1
            continue
        if n == "u":
            end = s.index("}", i)
            out.append(chr(int(s[i + 3 : end], 16)))
            i = end + 1
            continue
        out.append({"n": "\n", "t": "\t", "r": "\r", "0": "\0", "\\": "\\", '"': '"', "'": "'"}[n])
        i += 2
    return "".join(out)


JS_MARK = re.compile(r'(?<![A-Za-z0-9_$.])T\(\s*("(?:[^"\\]|\\.)*"|\'(?:[^\'\\]|\\.)*\')')
HTML_MARK = re.compile(r'\bdata-t(?:-[a-z-]+)?="([^"]*)"')


def js_string(lit):
    if lit[0] == "'":
        lit = '"' + lit[1:-1].replace('\\\'', "'").replace('"', '\\"') + '"'
    return json.loads(lit)


def web_keys(found):
    """The web viewer's words: T("...") in its script, data-t="..." in its page."""
    import html
    for name, mark in (("app.js", JS_MARK), ("index.html", HTML_MARK)):
        path = os.path.join(ROOT, "web", name)
        if not os.path.exists(path):
            continue
        text = open(path, encoding="utf-8").read()
        for m in mark.finditer(text):
            s = js_string(m.group(1)) if name == "app.js" else html.unescape(m.group(1))
            found.setdefault(s, "web/" + name)


def keys():
    found = {}
    web_keys(found)
    for dirpath, _, files in os.walk(os.path.join(ROOT, "src")):
        for f in sorted(files):
            if not f.endswith(".rs") or f == "tests.rs":
                continue
            path = os.path.join(dirpath, f)
            text = open(path, encoding="utf-8").read()
            # Tests speak English and are not shown to anyone.
            cut = text.find("#[cfg(test)]\nmod tests")
            if cut >= 0:
                text = text[:cut]
            for m in MARK.finditer(text):
                s = unescape(m.group(1)) if m.group(1) is not None else m.group(3)
                found.setdefault(s, os.path.relpath(path, ROOT))
    return found


def tag(s):
    """A short name for an English text that stays while others come and go."""
    import hashlib
    return hashlib.sha1(s.encode("utf-8")).hexdigest()[:8]


def slots(s):
    s = s.replace("{{", "").replace("}}", "")
    n, out = 0, []
    for m in SLOT.finditer(s):
        if m.group(1) == "":
            out.append(n)
            n += 1
        else:
            out.append(int(m.group(1)))
    return sorted(out)


TAG = re.compile(r"</?[a-z]+>")


def tags(s):
    """The markup a web text keeps (<b>, <a>), whatever the words between."""
    return sorted(TAG.findall(s))


def tables():
    for f in sorted(os.listdir(LANG)):
        if f.endswith(".json"):
            path = os.path.join(LANG, f)
            yield f[:-5], path, json.load(open(path, encoding="utf-8"))


def write(path, table, order):
    ordered = {k: table[k] for k in order if k in table}
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(ordered, fh, ensure_ascii=False, indent=1, sort_keys=False)
        fh.write("\n")


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else "check"
    k = keys()
    order = sorted(k, key=lambda s: s.lower())
    if cmd == "keys":
        json.dump(order, sys.stdout, ensure_ascii=False, indent=1)
        print()
    elif cmd == "todo":
        code = sys.argv[2]
        most = int(sys.argv[3]) if len(sys.argv) > 3 else len(order)
        table = json.load(open(os.path.join(LANG, code + ".json"), encoding="utf-8"))
        todo = {tag(s): s for s in order if not table.get(s)}
        json.dump(dict(list(todo.items())[:most]), sys.stdout, ensure_ascii=False, indent=1)
        print()
        print(f"# {min(most, len(todo))} of {len(todo)} missing", file=sys.stderr)
    elif cmd == "pairs":
        code = sys.argv[2]
        start = int(sys.argv[3]) if len(sys.argv) > 3 else 0
        most = int(sys.argv[4]) if len(sys.argv) > 4 else len(order)
        table = json.load(open(os.path.join(LANG, code + ".json"), encoding="utf-8"))
        json.dump({tag(s): [s, table.get(s, "")] for s in order[start : start + most]}, sys.stdout, ensure_ascii=False, indent=1)
        print()
        print(f"# {start}..{min(start + most, len(order))} of {len(order)}", file=sys.stderr)
    elif cmd == "put":
        code, src = sys.argv[2], sys.argv[3]
        path = os.path.join(LANG, code + ".json")
        table = json.load(open(path, encoding="utf-8"))
        given = json.load(open(src, encoding="utf-8"))
        by_tag = {tag(s): s for s in order}
        taken, bad = 0, []
        for n, text in given.items():
            en = by_tag.get(n)
            if en is None or not isinstance(text, str) or not text.strip():
                bad.append(f"{n} (no such text now, or empty)")
                continue
            if slots(en) != slots(text):
                bad.append(f"{n} (slots: {en!r} -> {text!r})")
                continue
            if tags(en) != tags(text):
                bad.append(f"{n} (keep the tags {tags(en)}: {en!r} -> {text!r})")
                continue
            table[en] = text
            taken += 1
        write(path, table, order)
        left = sum(1 for s in order if not table.get(s))
        print(f"{code}: took {taken}, refused {len(bad)}, {left} still missing")
        for b in bad[:20]:
            print("  refused", b)
    elif cmd == "sync":
        for code, path, table in tables():
            write(path, table, order)
    elif cmd == "check":
        bad = False
        print(f"{len(order)} words to translate")
        for code, path, table in tables():
            missing = [s for s in order if not table.get(s)]
            stale = [s for s in table if s not in k]
            wrong = [s for s in order if table.get(s) and (slots(s) != slots(table[s]) or tags(s) != tags(table[s]))]
            bad |= bool(missing or stale or wrong)
            print(f"{code:6} {len(order) - len(missing):5} done, {len(missing):5} missing, {len(stale):4} stale, {len(wrong):3} slots wrong")
            for s in wrong[:5]:
                print(f"         slots: {s!r} -> {table[s]!r}")
        sys.exit(1 if bad else 0)
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
