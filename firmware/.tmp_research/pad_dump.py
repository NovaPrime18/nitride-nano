#!/usr/bin/env python3
"""Dump full pad maps for given refs from a .kicad_pcb file."""
import re, sys

def block_end(src, start):
    depth = 0
    i = start
    while i < len(src):
        c = src[i]
        if c == '"':
            i += 1
            while i < len(src) and src[i] != '"':
                i += 1
        elif c == '(':
            depth += 1
        elif c == ')':
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return len(src) - 1

def dump(path, refs):
    src = open(path).read()
    for m in re.finditer(r'\(footprint "([^"]+)"', src):
        start = m.start()
        end = block_end(src, start)
        block = src[start:end]
        ref_m = re.search(r'\(property "Reference" "([^"]*)"', block)
        val_m = re.search(r'\(property "Value" "([^"]*)"', block)
        ref = ref_m.group(1) if ref_m else '???'
        if ref not in refs:
            continue
        print(f"### {ref} = {val_m.group(1) if val_m else '?'}  [{m.group(1)}]")
        for pm in re.finditer(r'\(pad "([^"]+)"', block):
            pstart = pm.start()
            pend = block_end(block, pstart)
            pblock = block[pstart:pend]
            nm = re.search(r'\(net "([^"]*)"\)', pblock)
            print(f"    pad {pm.group(1):>4} : {nm.group(1) if nm else '<no net>'}")

if __name__ == '__main__':
    path = sys.argv[1]
    refs = set(sys.argv[2:])
    dump(path, refs)