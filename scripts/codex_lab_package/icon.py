"""Generate the Codex Lab application icon without external dependencies."""

import binascii
import math
from pathlib import Path
import struct
import zlib


ICON_FILE_NAME = "CodexLab.icns"
_ICON_CHUNKS = (
    (b"ic10", 1024),
    (b"ic09", 512),
    (b"ic08", 256),
    (b"ic07", 128),
    (b"icp6", 64),
    (b"icp5", 32),
    (b"icp4", 16),
)


def write_codex_lab_icon(path: Path) -> None:
    chunks = []
    for chunk_type, size in _ICON_CHUNKS:
        png = _render_png(size)
        chunks.append(chunk_type + struct.pack(">I", len(png) + 8) + png)
    payload = b"".join(chunks)
    path.write_bytes(b"icns" + struct.pack(">I", len(payload) + 8) + payload)


def _render_png(size: int) -> bytes:
    rows = bytearray()
    offsets = (
        ((0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75))
        if size <= 256
        else ((0.5, 0.5),)
    )
    for pixel_y in range(size):
        rows.append(0)
        for pixel_x in range(size):
            samples = [
                _sample((pixel_x + offset_x) / size, (pixel_y + offset_y) / size)
                for offset_x, offset_y in offsets
            ]
            alpha_sum = sum(sample[3] for sample in samples)
            if alpha_sum == 0:
                rows.extend((0, 0, 0, 0))
                continue
            rows.extend(
                (
                    round(sum(sample[0] * sample[3] for sample in samples) / alpha_sum),
                    round(sum(sample[1] * sample[3] for sample in samples) / alpha_sum),
                    round(sum(sample[2] * sample[3] for sample in samples) / alpha_sum),
                    round(alpha_sum / len(samples)),
                )
            )

    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + _png_chunk(b"IHDR", header)
        + _png_chunk(b"IDAT", zlib.compress(bytes(rows), level=9))
        + _png_chunk(b"IEND", b"")
    )


def _png_chunk(chunk_type: bytes, payload: bytes) -> bytes:
    checksum = binascii.crc32(chunk_type)
    checksum = binascii.crc32(payload, checksum) & 0xFFFFFFFF
    return (
        struct.pack(">I", len(payload))
        + chunk_type
        + payload
        + struct.pack(">I", checksum)
    )


def _sample(x: float, y: float) -> tuple[int, int, int, int]:
    if not _inside_rounded_rect(x, y, 0.04, 0.04, 0.96, 0.96, 0.21):
        return (0, 0, 0, 0)

    blend = min(1.0, max(0.0, 0.34 * x + 0.66 * y))
    top = (25, 29, 57)
    bottom = (108, 78, 211)
    background = (
        round(top[0] + (bottom[0] - top[0]) * blend),
        round(top[1] + (bottom[1] - top[1]) * blend),
        round(top[2] + (bottom[2] - top[2]) * blend),
    )
    glow = max(0.0, 1.0 - math.hypot(x - 0.27, y - 0.77) / 0.55)
    background = (
        min(255, round(background[0] + 8 * glow)),
        min(255, round(background[1] + 25 * glow)),
        min(255, round(background[2] + 33 * glow)),
    )
    if not _inside_rounded_rect(x, y, 0.055, 0.055, 0.945, 0.945, 0.195):
        background = (
            min(255, background[0] + 18),
            min(255, background[1] + 18),
            min(255, background[2] + 18),
        )

    flask = _flask_color(x, y, background)
    if flask is not None:
        return (*flask, 255)
    return (*background, 255)


def _flask_color(
    x: float, y: float, background: tuple[int, int, int]
) -> tuple[int, int, int] | None:
    white = (247, 249, 255)
    cyan = (54, 220, 230)
    outer = (0.40 <= x <= 0.60 and 0.17 <= y <= 0.23) or (
        0.435 <= x <= 0.565 and 0.20 <= y <= 0.40
    )
    if 0.36 <= y <= 0.79:
        half_width = 0.065 + ((y - 0.36) / 0.43) * 0.275
        outer = outer or abs(x - 0.5) <= half_width
    if not outer:
        for bubble_x, bubble_y, radius in (
            (0.43, 0.49, 0.022),
            (0.58, 0.44, 0.017),
            (0.49, 0.39, 0.012),
        ):
            if math.hypot(x - bubble_x, y - bubble_y) <= radius:
                return cyan
        return None

    if 0.475 <= x <= 0.525 and 0.23 <= y <= 0.405:
        return background
    if 0.405 <= y <= 0.735:
        inner_half_width = 0.025 + ((y - 0.405) / 0.33) * 0.225
        if abs(x - 0.5) <= inner_half_width:
            liquid_line = 0.565 + 0.014 * math.sin((x - 0.5) * 31)
            return cyan if y >= liquid_line else background
    return white


def _inside_rounded_rect(
    x: float,
    y: float,
    left: float,
    top: float,
    right: float,
    bottom: float,
    radius: float,
) -> bool:
    nearest_x = min(max(x, left + radius), right - radius)
    nearest_y = min(max(y, top + radius), bottom - radius)
    return math.hypot(x - nearest_x, y - nearest_y) <= radius
