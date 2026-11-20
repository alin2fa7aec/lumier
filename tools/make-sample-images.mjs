#!/usr/bin/env node
// Generates sample images for local development, so the viewer can be exercised
// without pointing it at a real photo library.
//
// Writes PNGs directly — no image library needed, since the only thing being
// tested is that the wall lays out a mix of aspect ratios and honours ordering.
//
//   node tools/make-sample-images.mjs sample-photos [count]
//
// Each image carries a row of blocks matching its index, so the wall's sort
// order is verifiable at a glance.

import { deflateSync } from 'node:zlib'
import { mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

const CRC_TABLE = Uint32Array.from({ length: 256 }, (_, n) => {
  let c = n
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
  return c >>> 0
})

function crc32(buffer) {
  let c = 0xffffffff
  for (const byte of buffer) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8)
  return (c ^ 0xffffffff) >>> 0
}

function chunk(type, data) {
  const head = Buffer.alloc(4)
  head.writeUInt32BE(data.length)
  const body = Buffer.concat([Buffer.from(type, 'ascii'), data])
  const crc = Buffer.alloc(4)
  crc.writeUInt32BE(crc32(body))
  return Buffer.concat([head, body, crc])
}

function encodePng(width, height, paint) {
  const stride = width * 3
  // One filter byte (0 = none) per scanline, then RGB triples.
  const raw = Buffer.alloc(height * (stride + 1))
  for (let y = 0; y < height; y++) {
    const rowStart = y * (stride + 1)
    raw[rowStart] = 0
    for (let x = 0; x < width; x++) {
      const [r, g, b] = paint(x, y)
      raw.writeUInt8(r, rowStart + 1 + x * 3)
      raw.writeUInt8(g, rowStart + 2 + x * 3)
      raw.writeUInt8(b, rowStart + 3 + x * 3)
    }
  }

  const ihdr = Buffer.alloc(13)
  ihdr.writeUInt32BE(width, 0)
  ihdr.writeUInt32BE(height, 4)
  ihdr.writeUInt8(8, 8) // bit depth
  ihdr.writeUInt8(2, 9) // colour type: truecolour RGB
  // Remaining bytes stay zero: deflate, no filter, no interlace.

  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', deflateSync(raw, { level: 6 })),
    chunk('IEND', Buffer.alloc(0)),
  ])
}

/** Cycles through aspect ratios so the wall has to cope with a real mix. */
const SHAPES = [
  [1600, 1067], // 3:2 landscape
  [1067, 1600], // 3:2 portrait
  [1200, 1200], // square
  [2400, 1000], // panorama
  [1400, 1050], // 4:3 landscape
  [900, 1600], // 9:16 portrait
]

/** Converts an HSL-ish hue into RGB; enough variety to tell images apart. */
function hueToRgb(hue, lightness) {
  const f = (n) => {
    const k = (n + hue / 60) % 6
    const value = lightness - lightness * 0.55 * Math.max(-1, Math.min(k - 3, 5 - k, 1))
    return Math.round(Math.max(0, Math.min(1, value)) * 255)
  }
  return [f(5), f(3), f(1)]
}

const outDir = process.argv[2] ?? 'sample-photos'
const count = Number(process.argv[3] ?? 12)
if (!Number.isInteger(count) || count < 1) {
  console.error(`count must be a positive integer, got ${process.argv[3]}`)
  process.exit(1)
}

mkdirSync(outDir, { recursive: true })

for (let i = 1; i <= count; i++) {
  const [width, height] = SHAPES[(i - 1) % SHAPES.length]
  const hue = ((i - 1) * 360) / count
  const block = Math.round(Math.min(width, height) / 22)

  const png = encodePng(width, height, (x, y) => {
    // A row of `i` blocks in the top-left marks the index.
    const inMarkerRow = y >= block && y < block * 2
    const markerIndex = Math.floor((x - block) / (block * 1.5))
    if (inMarkerRow && x >= block && markerIndex < i && (x - block) % (block * 1.5) < block) {
      return [250, 250, 250]
    }
    const across = x / width
    const down = y / height
    return hueToRgb(hue, 0.3 + 0.45 * across * (1 - 0.35 * down))
  })

  const name = join(outDir, `photo-${String(i).padStart(2, '0')}.png`)
  writeFileSync(name, png)
  console.log(`${name}  ${width}x${height}  ${(png.length / 1024).toFixed(0)} KiB`)
}

console.log(`\n${count} images in ${outDir}`)
