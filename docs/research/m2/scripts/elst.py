# Print every elst (version, entries) and mvhd timescale in an MP4.
import struct, sys
def boxes(b, off, end):
    while off + 8 <= end:
        size, typ = struct.unpack('>I4s', b[off:off+8]); hdr = 8
        if size == 1: size = struct.unpack('>Q', b[off+8:off+16])[0]; hdr = 16
        if size == 0: size = end - off
        yield typ.decode('latin1'), off, off + hdr, off + size
        off += size
def walk(b, off, end, path):
    for typ, start, body, stop in boxes(b, off, end):
        p = path + [typ]
        if typ in ('moov', 'trak', 'edts', 'mdia', 'minf', 'stbl'):
            walk(b, body, stop, p)
        elif typ == 'mvhd':
            v = b[body]; ts = struct.unpack('>I', b[body+12:body+16] if v == 0 else b[body+20:body+24])[0]
            print('mvhd timescale', ts)
        elif typ == 'hdlr':
            print('track', b[body+8:body+12].decode())
        elif typ == 'elst':
            v = b[body]; n = struct.unpack('>I', b[body+4:body+8])[0]; o = body + 8; ents = []
            for _ in range(n):
                if v == 0: d, t = struct.unpack('>Ii', b[o:o+8]); o += 12
                else: d, t = struct.unpack('>Qq', b[o:o+16]); o += 20
                ents.append((d, t))
            print('  elst v%d' % v, ents)
for f in sys.argv[1:]:
    b = open(f, 'rb').read(); print('==', f); walk(b, 0, len(b), [])
