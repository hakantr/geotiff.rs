import fs from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [jsRootArg, fixtureRootArg] = process.argv.slice(2);
if (!jsRootArg || !fixtureRootArg) {
  throw new Error('usage: node js_errors_oracle.mjs <geotiff.js-root> <fixture-root>');
}
const jsRoot = path.resolve(jsRootArg);
const fixtureRoot = path.resolve(fixtureRootArg);
const moduleUrl = (relative) => pathToFileURL(path.join(jsRoot, 'dist-module', relative));
const geotiff = await import(moduleUrl('geotiff.js'));
const compression = await import(moduleUrl('compression/index.js'));
const { BaseSource } = await import(moduleUrl('source/basesource.js'));
const packageJson = JSON.parse(await fs.readFile(path.join(jsRoot, 'package.json'), 'utf8'));
const fixtureBytes = await fs.readFile(path.join(fixtureRoot, 'tiled-gray-i1.tif'));

function exactArrayBuffer(bytes) {
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
}

function normalize(value) {
  if (value === undefined) return { $undefined: true };
  if (value === null || typeof value === 'string' || typeof value === 'boolean' || typeof value === 'number') return value;
  if (ArrayBuffer.isView(value)) return Array.from(value);
  if (value instanceof ArrayBuffer || value instanceof SharedArrayBuffer) return Array.from(new Uint8Array(value));
  if (Array.isArray(value)) return value.map(normalize);
  if (typeof value === 'object') return Object.fromEntries(Object.keys(value).sort().map((key) => [key, normalize(value[key])]));
  return String(value);
}

async function capture(operation) {
  try {
    return { ok: normalize(await operation()) };
  } catch (error) {
    return {
      error: {
        name: error?.constructor?.name || error?.name || 'Error',
        message: String(error?.message ?? error),
      },
    };
  }
}

function header({ byteOrder = 'II', magic = 42, offsetSize = 8, reserved = 0 } = {}) {
  const bytes = new Uint8Array(1024);
  bytes[0] = byteOrder.charCodeAt(0);
  bytes[1] = byteOrder.charCodeAt(1);
  const little = byteOrder === 'II';
  const view = new DataView(bytes.buffer);
  view.setUint16(2, magic, little);
  if (magic === 43) {
    view.setUint16(4, offsetSize, little);
    view.setUint16(6, reserved, little);
  }
  return bytes.buffer;
}

function withShortTag(source, tag, value) {
  const output = new Uint8Array(source);
  const view = new DataView(output.buffer);
  const little = view.getUint16(0, false) === 0x4949;
  const ifdOffset = view.getUint32(4, little);
  const count = view.getUint16(ifdOffset, little);
  for (let index = 0; index < count; index += 1) {
    const entry = ifdOffset + 2 + (index * 12);
    if (view.getUint16(entry, little) === tag) {
      view.setUint16(entry + 8, value, little);
      return output.buffer;
    }
  }
  throw new Error(`tag ${tag} not found`);
}

const headerErrors = {
  invalidByteOrder: await capture(() => geotiff.fromArrayBuffer(header({ byteOrder: 'ZZ' }))),
  invalidMagic: await capture(() => geotiff.fromArrayBuffer(header({ magic: 99 }))),
  invalidBigTiffOffsetSize: await capture(() => geotiff.fromArrayBuffer(header({ magic: 43, offsetSize: 4 }))),
  nonZeroBigTiffReserved: await capture(async () => {
    const file = await geotiff.fromArrayBuffer(header({ magic: 43, reserved: 1 }));
    await file.close();
    return true;
  }),
  missingFile: await capture(() => geotiff.fromFile(path.join(fixtureRoot, '__missing__.tif'))),
};

const validFile = await geotiff.fromArrayBuffer(exactArrayBuffer(fixtureBytes));
const validImage = await validFile.getImage(0);
const directory = validImage.getFileDirectory();
const compressionId = directory.getValue('Compression') || 1;
const decoderParameters = await compression.getDecoderParameters(compressionId, directory);
const decoder = await geotiff.getDecoder(compressionId, decoderParameters);

const datasetErrors = {
  imageIndex: await capture(() => validFile.getImage(5)),
  requestIfdIndex: await capture(() => validFile.requestIFD(5)),
};

const imageErrors = {
  reversedWindow: await capture(() => validImage.readRasters({ window: [5, 5, 2, 2] })),
  invalidSample: await capture(() => validImage.readRasters({ samples: [99] })),
  interleavedFillArray: await capture(() => validImage.readRasters({
    interleave: true,
    window: [-1, -1, 2, 2],
    fillValue: [1],
  })),
  unknownResample: await capture(() => validImage.readRasters({
    interleave: true,
    width: 5,
    height: 5,
    resampleMethod: 'oracle-unknown',
  })),
  invalidReaderSample: await capture(() => validImage.getReaderForSample(99)),
  invalidArraySample: await capture(() => validImage.getArrayForSample(99, 1)),
  invalidTileSample: await capture(async () => {
    const block = await validImage.getTileOrStrip(0, 0, 99, decoder);
    return { x: block.x, y: block.y, sample: block.sample, dataLength: block.data.byteLength };
  }),
  invalidTileCoordinates: await capture(() => validImage.getTileOrStrip(999, 999, 0, decoder)),
};

const invalidPlanarFile = await geotiff.fromArrayBuffer(withShortTag(fixtureBytes, 284, 3));
imageErrors.invalidPlanarConfiguration = await capture(() => invalidPlanarFile.getImage(0));
await invalidPlanarFile.close();

const unsupportedRgbFile = await geotiff.fromArrayBuffer(withShortTag(fixtureBytes, 262, 4));
const unsupportedRgbImage = await unsupportedRgbFile.getImage(0);
imageErrors.unsupportedPhotometric = await capture(() => unsupportedRgbImage.readRGB());
await unsupportedRgbFile.close();

const unsupportedFormatFile = await geotiff.fromArrayBuffer(withShortTag(fixtureBytes, 339, 4));
const unsupportedFormatImage = await unsupportedFormatFile.getImage(0);
imageErrors.unsupportedSampleFormat = await capture(() => unsupportedFormatImage.readRasters({ interleave: true }));
await unsupportedFormatFile.close();

const geoBytes = geotiff.writeArrayBuffer(new Uint8Array(16), { width: 4, height: 4 });
const geoFile = await geotiff.fromArrayBuffer(geoBytes);
const optionErrors = {
  bboxAndWindow: await capture(() => geoFile.readRasters({
    window: [0, 0, 2, 2],
    bbox: [-180, 0, 0, 90],
  })),
  widthAndResX: await capture(() => geoFile.readRasters({ width: 2, resX: 1 })),
  heightAndResY: await capture(() => geoFile.readRasters({ height: 2, resY: 1 })),
};

const codecErrors = {
  unknownCompression: await capture(() => geotiff.getDecoder(64000, {})),
  oldJpeg: await capture(() => geotiff.getDecoder(6, {})),
};

const abstractSource = new BaseSource();
const sourceErrors = {
  fetchSlice: await capture(() => abstractSource.fetchSlice({ offset: 1, length: 2 })),
  fetch: await capture(() => abstractSource.fetch([{ offset: 1, length: 2 }])),
};

const writerErrors = {
  empty: await capture(() => geotiff.writeArrayBuffer([], {})),
  missingHeight: await capture(() => geotiff.writeArrayBuffer([1, 2], { width: 2 })),
  missingWidth: await capture(() => geotiff.writeArrayBuffer([1, 2], { height: 1 })),
  tiledSamples: await capture(() => geotiff.writeArrayBuffer(new Uint8Array([1, 2, 3, 4]), {
    width: 2,
    height: 2,
    TileWidth: 2,
    TileLength: 2,
    TileByteCounts: [4],
  })),
  tiledDimensions: await capture(() => geotiff.writeArrayBuffer(new Uint8Array([1, 2, 3, 4]), {
    width: 2,
    height: 2,
    SamplesPerPixel: 1,
    TileByteCounts: [4],
  })),
  geoAsciiType: await capture(() => geotiff.writeArrayBuffer(new Uint8Array([1]), {
    width: 1,
    height: 1,
    GeoAsciiParams: 42,
  })),
  geoDoubleType: await capture(() => geotiff.writeArrayBuffer(new Uint8Array([1]), {
    width: 1,
    height: 1,
    GeoDoubleParams: new Float64Array([1]),
  })),
  geoKeyType: await capture(() => geotiff.writeArrayBuffer(new Uint8Array([1]), {
    width: 1,
    height: 1,
    GeographicTypeGeoKey: '4326',
  })),
  ifdTooLarge: await capture(() => geotiff.writeArrayBuffer(new Uint8Array([1]), {
    width: 1,
    height: 1,
    SamplesPerPixel: 1,
    TileWidth: 1,
    TileLength: 1,
    TileByteCounts: new Array(1000).fill(1),
  })),
};

await validFile.close();
await geoFile.close();

process.stdout.write(`${JSON.stringify({
  reference: { version: packageJson.version },
  headerErrors,
  datasetErrors,
  imageErrors,
  optionErrors,
  codecErrors,
  sourceErrors,
  writerErrors,
})}\n`);
