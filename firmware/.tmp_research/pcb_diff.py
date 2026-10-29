#!/usr/bin/env python3
"""Diff KiCad PCB footprints (pads->nets, values) between two .kicad_pcb files. KiCad 8 format."""
import re, sys, json

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

def parse_pcb(path):
    src = open(path).read()
    footprints = {}
    for m in re.finditer(r'\(footprint "([^"]+)"', src):
        fp_lib = m.group(1)
        start = m.start()
        end = block_end(src, start)
        block = src[start:end]
        ref_m = re.search(r'\(property "Reference" "([^"]*)"', block)
        val_m = re.search(r'\(property "Value" "([^"]*)"', block)
        ref = ref_m.group(1) if ref_m else '???'
        pads = {}
        for pm in re.finditer(r'\(pad "([^"]+)"', block):
            pstart = pm.start()
            pend = block_end(block, pstart)
            pblock = block[pstart:pend]
            nm = re.search(r'\(net "([^"]*)"\)', pblock)
            pads[pm.group(1)] = nm.group(1) if nm else None
        footprints[ref] = {'value': val_m.group(1) if val_m else '',
                           'fp': fp_lib,
                           'pads': pads}
    return footprints

def main():
    a = parse_pcb(sys.argv[1])
    b = parse_pcb(sys.argv[2])
    refs_a, refs_b = set(a), set(b)
    report = {'added': {}, 'removed': {}, 'value_changed': {}, 'net_changed': {}}
    for r in sorted(refs_b - refs_a):
        report['added'][r] = {'value': b[r]['value'], 'fp': b[r]['fp'], 'pads': b[r]['pads']}
    for r in sorted(refs_a - refs_b):
        report['removed'][r] = {'value': a[r]['value'], 'fp': a[r]['fp'], 'pads': a[r]['pads']}
    for r in sorted(refs_a & refs_b):
        if a[r]['value'] != b[r]['value']:
            report['value_changed'][r] = {'r2': a[r]['value'], 'r3': b[r]['value'], 'fp': b[r]['fp']}
        pdiff = {}
        for pad in set(a[r]['pads']) | set(b[r]['pads']):
            n1 = a[r]['pads'].get(pad)
            n2 = b[r]['pads'].get(pad)
            if n1 != n2:
                pdiff[pad] = {'r2': n1, 'r3': n2}
        if pdiff:
            report['net_changed'][r] = {'value': b[r]['value'], 'fp': b[r]['fp'], 'pads': pdiff}
    print(json.dumps(report, indent=1))

if __name__ == '__main__':
    main()