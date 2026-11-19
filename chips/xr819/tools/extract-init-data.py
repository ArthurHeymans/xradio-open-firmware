#!/usr/bin/env python3
"""Prepare local build inputs from the identified vendor XR01 image (no download)."""
import argparse
from hashlib import sha256
from pathlib import Path

CONTAINER = '3e2462d476c9dfcb907cda1ba81d0a6d1bbee5e3207bdc1911ec5042d96fdfca'
# File offsets in this exact container, not runtime addresses.
SECTIONS = (
    ('engine-mode1-init.bin', 0x1d6c4, 430),
    ('phy-init-ab81000.bin', 0x1f084, 1024),
    ('phy-init-ab88400.bin', 0x1f490, 816),
    ('phy-init-ab88800.bin', 0x1f7cc, 816),
    ('phy-init-ab88c00.bin', 0x1fb08, 816),
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('image', type=Path)
    parser.add_argument('--output', type=Path, default=Path(__file__).resolve().parents[1] / 'data')
    args = parser.parse_args()
    image = args.image.read_bytes()
    if sha256(image).hexdigest() != CONTAINER:
        parser.exit(1, 'Unexpected container hash; no files written.\n')
    files = [(args.output / name, image[offset:offset + size]) for name, offset, size in SECTIONS]
    for path, data in files:
        if path.exists() and path.read_bytes() != data:
            parser.exit(1, f'Refusing to overwrite differing {path}; no files written.\n')
    args.output.mkdir(parents=True, exist_ok=True)
    for path, data in files:
        if not path.exists():
            with path.open('xb') as out:
                out.write(data)
        print(f'{sha256(data).hexdigest()}  {path.name}')


if __name__ == '__main__':
    main()
