"""Pack PNG files into one Windows .ico (PNG-compressed entries, Vista and later).

    python3 ico.py out.ico 16.png 32.png 48.png 256.png
"""

import struct
import sys

out, pngs = sys.argv[1], sys.argv[2:]
images = [open(p, "rb").read() for p in pngs]
entries, offset = b"", 6 + 16 * len(images)
for data in images:
    width, height = struct.unpack(">II", data[16:24])  # from the IHDR chunk
    entries += struct.pack("<BBBBHHII", width % 256, height % 256, 0, 0, 1, 32, len(data), offset)
    offset += len(data)
with open(out, "wb") as f:
    f.write(struct.pack("<HHH", 0, 1, len(images)) + entries + b"".join(images))
