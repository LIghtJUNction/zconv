"""Read LLVM .stack_sizes entries from a little-endian ELF64 relocatable file.

Uses only Python's standard library and the existing GNU c++filt executable.
Reports compiler-emitted single-frame sizes, not a call-chain memory bound.
"""
import csv
import json
import re
import struct
import subprocess
import sys
from pathlib import Path

path = Path(sys.argv[1])
blob = path.read_bytes()
if blob[:6] != b"\x7fELF\x02\x01":
    raise SystemExit("expected little-endian ELF64")
header = struct.unpack_from("<16sHHIQQQIHHHHHH", blob)
if header[1] != 1:
    raise SystemExit("expected a relocatable object")
section_offset, entry_size, section_count, names_index = header[6], header[11], header[12], header[13]
sections = [struct.unpack_from("<IIQQQQIIQQ", blob, section_offset + i * entry_size) for i in range(section_count)]

def contents(section):
    return blob[section[4]:section[4] + section[5]]

def cstring(data, offset):
    return data[offset:data.index(0, offset)].decode("utf-8")

section_names = contents(sections[names_index])
names = [cstring(section_names, section[0]) for section in sections]
rows = []
for section_index, section in enumerate(sections):
    if section[1] != 4 or names[section[7]] != ".stack_sizes":
        continue
    stack_data = contents(sections[section[7]])
    symbol_section = sections[section[6]]
    symbols = contents(symbol_section)
    symbol_names = contents(sections[symbol_section[6]])
    relocations = contents(section)
    for offset in range(0, len(relocations), section[9]):
        address_offset, info, addend = struct.unpack_from("<QQq", relocations, offset)
        if info & 0xffffffff != 1:
            raise SystemExit("unexpected stack-size relocation type")
        symbol = struct.unpack_from("<IBBHQQ", symbols, (info >> 32) * symbol_section[9])
        mangled = cstring(symbol_names, symbol[0]) if symbol[0] else names[symbol[3]].removeprefix(".text.")
        frame, shift, cursor = 0, 0, address_offset + 8
        while True:
            byte = stack_data[cursor]
            cursor += 1
            frame |= (byte & 0x7f) << shift
            if not byte & 0x80:
                break
            shift += 7
        rows.append({"mangled": mangled, "frame_bytes": frame, "text_section": names[symbol[3]], "relocation_addend": addend})

demangled = subprocess.run(["c++filt", "-s", "rust"], input="\n".join(row["mangled"] for row in rows), text=True, check=True, capture_output=True).stdout.splitlines()
if len(demangled) != len(rows):
    raise SystemExit("demangler row count mismatch")
for row, name in zip(rows, demangled):
    row["function"] = name
rows.sort(key=lambda row: (-row["frame_bytes"], row["function"]))
path.with_suffix(".stack-sizes.json").write_text(json.dumps(rows, indent=2) + "\n")
with path.with_suffix(".stack-sizes.csv").open("w", newline="") as output:
    writer = csv.DictWriter(output, fieldnames=["function", "frame_bytes", "mangled", "text_section", "relocation_addend"])
    writer.writeheader()
    writer.writerows(rows)

def normalized(name):
    return re.sub(r"\[[0-9a-f]+\]", "", name).replace(">::", "::").lstrip("<")

selected = [row for row in rows if any(key in normalized(row["function"]) for key in [
    "zconv::json::parse", "zconv::json::Parser::value", "zconv::json::Parser::array",
    "zconv::json::Parser::object", "zconv::json::Parser::string", "zconv::json::Parser::unique_keys",
    "zconv::convert::convert", "zconv::direct::convert", "zconv::direct::request", "zconv::direct::response",
    "zconv::protocol::decode::decode_request", "zconv::protocol::decode::decode_response",
    "zconv::protocol::encode::encode_request", "zconv::protocol::encode::encode_response",
    "zconv::stream::StreamConverter::step", "zconv::stream::process",
])]
print(json.dumps({"entries": len(rows), "selected": [{"function": normalized(row["function"]), "frame_bytes": row["frame_bytes"]} for row in selected], "largest_frames": [{"function": normalized(row["function"]), "frame_bytes": row["frame_bytes"]} for row in rows[:10]]}, indent=2))
