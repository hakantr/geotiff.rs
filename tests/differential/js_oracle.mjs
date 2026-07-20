import crypto from 'node:crypto';
import fs from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [jsRootArg, fixtureRootArg, casesArg] = process.argv.slice(2);
if (!jsRootArg || !fixtureRootArg || !casesArg) {
  throw new Error('usage: node js_oracle.mjs <geotiff.js-root> <fixture-root> <cases.json>');
}

const jsRoot = path.resolve(jsRootArg);
const fixtureRoot = path.resolve(fixtureRootArg);
const cases = JSON.parse(await fs.readFile(path.resolve(casesArg), 'utf8'));
const geotiff = await import(pathToFileURL(path.join(jsRoot, 'dist-module/geotiff.js')));
const compression = await import(
  pathToFileURL(path.join(jsRoot, 'dist-module/compression/index.js'))
);
const packageJson = JSON.parse(await fs.readFile(path.join(jsRoot, 'package.json'), 'utf8'));

function normalizedNumber(value) {
  if (Number.isNaN(value)) return { $number: 'NaN' };
  if (value === Infinity) return { $number: 'Infinity' };
  if (value === -Infinity) return { $number: '-Infinity' };
  if (Object.is(value, -0)) return { $number: '-0' };
  return value;
}

function normalize(value) {
  if (value === undefined) return { $undefined: true };
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return value;
  if (typeof value === 'number') return normalizedNumber(value);
  if (typeof value === 'bigint') return { $bigint: value.toString() };
  if (ArrayBuffer.isView(value)) return Array.from(value, normalize);
  if (value instanceof ArrayBuffer || value instanceof SharedArrayBuffer) {
    return Array.from(new Uint8Array(value));
  }
  if (Array.isArray(value)) return value.map(normalize);
  if (value instanceof Map) {
    return Object.fromEntries(
      [...value.entries()]
        .map(([key, entry]) => [String(key), normalize(entry)])
        .sort(([left], [right]) => left.localeCompare(right)),
    );
  }
  if (typeof value === 'object') {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, normalize(value[key])]),
    );
  }
  return String(value);
}

function typedBytes(array) {
  const name = array.constructor.name;
  const elementBytes = array.BYTES_PER_ELEMENT;
  const bytes = new Uint8Array(array.length * elementBytes);
  const view = new DataView(bytes.buffer);
  for (let index = 0; index < array.length; index += 1) {
    const offset = index * elementBytes;
    const value = array[index];
    switch (name) {
      case 'Int8Array': view.setInt8(offset, value); break;
      case 'Uint8Array':
      case 'Uint8ClampedArray': view.setUint8(offset, value); break;
      case 'Int16Array': view.setInt16(offset, value, true); break;
      case 'Uint16Array': view.setUint16(offset, value, true); break;
      case 'Int32Array': view.setInt32(offset, value, true); break;
      case 'Uint32Array': view.setUint32(offset, value, true); break;
      case 'BigInt64Array': view.setBigInt64(offset, value, true); break;
      case 'BigUint64Array': view.setBigUint64(offset, value, true); break;
      case 'Float32Array': view.setFloat32(offset, value, true); break;
      case 'Float64Array': view.setFloat64(offset, value, true); break;
      default: throw new Error(`unsupported oracle typed array: ${name}`);
    }
  }
  return bytes;
}

function typedDiagnosticValue(array, index) {
  const value = array[index];
  if (array instanceof Float32Array) {
    const buffer = new ArrayBuffer(4);
    const view = new DataView(buffer);
    view.setFloat32(0, value, true);
    return { $float32Bits: view.getUint32(0, true).toString(16).padStart(8, '0') };
  }
  if (array instanceof Float64Array) {
    const buffer = new ArrayBuffer(8);
    const view = new DataView(buffer);
    view.setFloat64(0, value, true);
    return { $float64Bits: view.getBigUint64(0, true).toString(16).padStart(16, '0') };
  }
  return normalize(value);
}

function typedSummary(array, includeValues = false) {
  const bytes = typedBytes(array);
  const result = {
    type: array.constructor.name,
    length: array.length,
    sha256: crypto.createHash('sha256').update(bytes).digest('hex'),
    first: Array.from(
      { length: Math.min(array.length, 8) },
      (_, index) => typedDiagnosticValue(array, index),
    ),
    last: Array.from(
      { length: Math.min(array.length, 8) },
      (_, index) => typedDiagnosticValue(array, array.length - Math.min(array.length, 8) + index),
    ),
  };
  if (includeValues) result.values = Array.from(array, normalize);
  return result;
}

function rasterSummary(result, includeValues = false) {
  if (ArrayBuffer.isView(result)) {
    return {
      shape: 'interleaved',
      width: result.width,
      height: result.height,
      data: typedSummary(result, includeValues),
    };
  }
  return {
    shape: 'bands',
    width: result.width,
    height: result.height,
    bands: result.map((band) => typedSummary(band, includeValues)),
  };
}

async function capture(operation) {
  try {
    return { ok: normalize(await operation()) };
  } catch (error) {
    return {
      error: {
        name: error?.constructor?.name || 'Error',
        message: String(error?.message ?? error),
      },
    };
  }
}

function readerKind(image, sample) {
  image.getReaderForSample(sample);
  const format = image.getSampleFormat(sample);
  const bits = image.getBitsPerSample(sample);
  if (format === 1 && bits <= 8) return 'uint8';
  if (format === 1 && bits <= 16) return 'uint16';
  if (format === 1 && bits <= 32) return 'uint32';
  if (format === 2 && bits <= 8) return 'int8';
  if (format === 2 && bits <= 16) return 'int16';
  if (format === 2 && bits <= 32) return 'int32';
  if (format === 3 && bits === 16) return 'float16';
  if (format === 3 && bits === 32) return 'float32';
  if (format === 3 && bits === 64) return 'float64';
  return 'unsupported';
}

async function directoryValues(directory) {
  const identifiers = new Set([
    ...directory.actualizedFields.keys(),
    ...directory.deferredFields.keys(),
    ...directory.deferredArrays.keys(),
  ]);
  const values = {};
  for (const identifier of [...identifiers].sort((left, right) => Number(left) - Number(right))) {
    values[String(identifier)] = normalize(await directory.loadValue(identifier));
  }
  return values;
}

async function directoryIndexedValues(directory) {
  return {
    bitsPerSample0: await capture(() => directory.loadValueIndexed('BitsPerSample', 0)),
    bitsPerSampleOutOfBounds: await capture(
      () => directory.loadValueIndexed('BitsPerSample', 999),
    ),
    imageWidthScalar: await capture(() => directory.loadValueIndexed('ImageWidth', 0)),
    software0: await capture(() => directory.loadValueIndexed('Software', 0)),
    softwareOutOfBounds: await capture(() => directory.loadValueIndexed('Software', 999)),
    xResolutionNumerator: await capture(() => directory.loadValueIndexed('XResolution', 0)),
    xResolutionDenominator: await capture(() => directory.loadValueIndexed('XResolution', 1)),
    xResolutionOutOfBounds: await capture(() => directory.loadValueIndexed('XResolution', 2)),
    missingTag: await capture(() => directory.loadValueIndexed(65000, 0)),
  };
}

async function metadataForFixture(name) {
  const file = await geotiff.fromFile(path.join(fixtureRoot, name));
  try {
    const imageCount = await file.getImageCount();
    const image = await file.getImage(0);
    const directory = image.getFileDirectory();
    const samples = image.getSamplesPerPixel();
    const tileHeight = image.getTileHeight();
    const blockRows = tileHeight > 0 ? Math.ceil(image.getHeight() / tileHeight) : 0;
    const sampleInfo = [];
    for (let sample = 0; sample < samples; sample += 1) {
      sampleInfo.push({
        bits: image.getBitsPerSample(sample),
        format: image.getSampleFormat(sample),
        byteSize: image.getSampleByteSize(sample),
        reader: readerKind(image, sample),
        arrayType: image.getArrayForSample(sample, 3).constructor.name,
      });
    }
    const gdalBySample = [];
    for (let sample = 0; sample < samples; sample += 1) {
      gdalBySample.push(await capture(() => image.getGDALMetadata(sample)));
    }
    const values = await directoryValues(directory);
    return {
      dataset: {
        imageCount,
        bigTiff: file.bigTiff,
        littleEndian: file.littleEndian,
        ghostValues: await capture(() => file.getGhostValues()),
      },
      image: {
        width: image.getWidth(),
        height: image.getHeight(),
        samplesPerPixel: samples,
        tiled: image.isTiled,
        planarConfiguration: image.planarConfiguration,
        tileWidth: image.getTileWidth(),
        tileHeight,
        blockWidth: image.getBlockWidth(),
        firstBlockHeight: blockRows > 0 ? image.getBlockHeight(0) : 0,
        lastBlockHeight: blockRows > 0 ? image.getBlockHeight(blockRows - 1) : 0,
        bytesPerPixel: await capture(() => image.getBytesPerPixel()),
        samples: sampleInfo,
        geoKeys: await capture(() => image.getGeoKeys()),
        tiePoints: await capture(() => image.getTiePoints()),
        gdalMetadata: await capture(() => image.getGDALMetadata()),
        gdalMetadataBySample: gdalBySample,
        gdalNoData: normalize(image.getGDALNoData()),
        origin: await capture(() => image.getOrigin()),
        resolution: await capture(() => image.getResolution()),
        pixelIsArea: image.pixelIsArea(),
        boundingBox: await capture(() => image.getBoundingBox(false)),
        tilegridBoundingBox: await capture(() => image.getBoundingBox(true)),
      },
      directory: {
        nextIfdByteOffset: directory.nextIFDByteOffset,
        values,
        indexed: await directoryIndexedValues(directory),
        geoKeys: await capture(() => directory.parseGeoKeyDirectory()),
        object: normalize(directory.toObject()),
      },
    };
  } finally {
    await file.close();
  }
}

function jsOptions(options) {
  const result = { ...options };
  delete result.packedSampleMode;
  return result;
}

async function rasterCase(testCase, rgb = false) {
  const file = await geotiff.fromFile(path.join(fixtureRoot, testCase.fixture));
  try {
    const image = await file.getImage(0);
    const operation = rgb ? image.readRGB.bind(image) : image.readRasters.bind(image);
    const includeValues = testCase.comparison === 'numericTolerance';
    try {
      const result = await operation(jsOptions(testCase.options || {}));
      return { ok: rasterSummary(result, includeValues) };
    } catch (error) {
      return {
        error: {
          name: error?.constructor?.name || 'Error',
          message: String(error?.message ?? error),
        },
      };
    }
  } finally {
    await file.close();
  }
}

async function blockCase(testCase) {
  const file = await geotiff.fromFile(path.join(fixtureRoot, testCase.fixture));
  try {
    const image = await file.getImage(0);
    const directory = image.getFileDirectory();
    const compressionId = directory.getValue('Compression');
    const parameters = await compression.getDecoderParameters(compressionId, directory);
    const decoder = await geotiff.getDecoder(compressionId, parameters);
    const { x, y, sample } = testCase.options;
    try {
      const result = await image.getTileOrStrip(x, y, sample, decoder);
      return {
        ok: {
          x: result.x,
          y: result.y,
          sample: result.sample,
          data: typedSummary(new Uint8Array(result.data)),
        },
      };
    } catch (error) {
      return {
        error: {
          name: error?.constructor?.name || 'Error',
          message: String(error?.message ?? error),
        },
      };
    }
  } finally {
    await file.close();
  }
}

const classNames = [
  'GeoTIFF',
  'MultiGeoTIFF',
  'GeoTIFFImage',
  'ImageFileDirectory',
  'Pool',
  'BaseClient',
  'BaseResponse',
  'BaseDecoder',
];

const output = {
  reference: {
    version: packageJson.version,
  },
  surface: {
    exports: Object.keys(geotiff).sort(),
    prototypes: Object.fromEntries(classNames.map((name) => [
      name,
      Object.getOwnPropertyNames(geotiff[name].prototype)
        .filter((method) => method !== 'constructor')
        .sort(),
    ])),
  },
  metadata: {},
  defaultRasters: {},
  rasterCases: {},
  blockCases: {},
  rgbCases: {},
};

for (const fixture of cases.metadataFixtures) {
  output.metadata[fixture] = await metadataForFixture(fixture);
}
for (const fixture of cases.defaultRasterFixtures) {
  output.defaultRasters[fixture] = await rasterCase({ fixture, options: {} });
}
for (const testCase of cases.rasterCases) {
  output.rasterCases[testCase.id] = await rasterCase(testCase);
}
for (const testCase of cases.blockCases) {
  output.blockCases[testCase.id] = await blockCase(testCase);
}
for (const testCase of cases.rgbCases) {
  output.rgbCases[testCase.id] = await rasterCase(testCase, true);
}

process.stdout.write(`${JSON.stringify(output)}\n`);
