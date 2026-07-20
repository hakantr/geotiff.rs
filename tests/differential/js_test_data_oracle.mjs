import crypto from 'node:crypto';
import fs from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [jsRootArg, corpusRootArg] = process.argv.slice(2);
if (!jsRootArg || !corpusRootArg) {
  throw new Error('usage: node js_test_data_oracle.mjs <geotiff.js-root> <test-data-root>');
}

const jsRoot = path.resolve(jsRootArg);
const corpusRoot = path.resolve(corpusRootArg);
const filesRoot = path.join(corpusRoot, 'files');
const geotiff = await import(pathToFileURL(path.join(jsRoot, 'dist-module/geotiff.js')));
const compression = await import(
  pathToFileURL(path.join(jsRoot, 'dist-module/compression/index.js'))
);
const packageJson = JSON.parse(await fs.readFile(path.join(jsRoot, 'package.json'), 'utf8'));

const FULL_SAMPLE_LIMIT = 4_000_000;

function normalizedNumber(value) {
  if (Number.isNaN(value)) return { $number: 'NaN' };
  if (value === Infinity) return { $number: 'Infinity' };
  if (value === -Infinity) return { $number: '-Infinity' };
  if (Object.is(value, -0)) return { $number: '-0' };
  if (Number.isSafeInteger(value)) return value;
  const buffer = new ArrayBuffer(8);
  const view = new DataView(buffer);
  view.setFloat64(0, value, true);
  return { $float64Bits: view.getBigUint64(0, true).toString(16).padStart(16, '0') };
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
      Object.keys(value).sort().map((key) => [key, normalize(value[key])]),
    );
  }
  return String(value);
}

async function capture(operation) {
  try {
    return { ok: normalize(await operation()) };
  } catch {
    // Corpus failures are compared by success/error state. Exact public
    // validation messages have their own adversarial differential matrix.
    return { error: true };
  }
}

function typedBytes(array) {
  const name = array.constructor.name;
  const bytes = new Uint8Array(array.length * array.BYTES_PER_ELEMENT);
  const view = new DataView(bytes.buffer);
  for (let index = 0; index < array.length; index += 1) {
    const offset = index * array.BYTES_PER_ELEMENT;
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
      default: throw new Error(`unsupported corpus typed array: ${name}`);
    }
  }
  return bytes;
}

function diagnosticValue(array, index) {
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

function typedSummary(array) {
  const bytes = typedBytes(array);
  const edge = Math.min(array.length, 4);
  return {
    type: array.constructor.name,
    length: array.length,
    sha256: crypto.createHash('sha256').update(bytes).digest('hex'),
    first: Array.from({ length: edge }, (_, index) => diagnosticValue(array, index)),
    last: Array.from(
      { length: edge },
      (_, index) => diagnosticValue(array, array.length - edge + index),
    ),
  };
}

function rasterSummary(result) {
  if (ArrayBuffer.isView(result)) {
    return {
      shape: 'interleaved',
      width: result.width,
      height: result.height,
      data: typedSummary(result),
    };
  }
  return {
    shape: 'bands',
    width: result.width,
    height: result.height,
    bands: result.map(typedSummary),
  };
}

function arrayValueSummary(value) {
  if (Array.isArray(value)) {
    const strings = value.map((entry) => String(entry));
    return {
      type: 'Array',
      length: value.length,
      sha256: crypto.createHash('sha256').update(strings.join('\0')).digest('hex'),
      first: normalize(value.slice(0, 4)),
      last: normalize(value.slice(Math.max(0, value.length - 4))),
    };
  }
  return typedSummary(value);
}

function directoryValueSummary(value) {
  if (Array.isArray(value) || ArrayBuffer.isView(value)) return arrayValueSummary(value);
  return normalize(value);
}

async function directorySummary(directory) {
  const identifiers = new Set([
    ...directory.actualizedFields.keys(),
    ...directory.deferredFields.keys(),
    ...directory.deferredArrays.keys(),
  ]);
  const output = {};
  for (const identifier of [...identifiers].sort((left, right) => Number(left) - Number(right))) {
    output[String(identifier)] = directoryValueSummary(await directory.loadValue(identifier));
  }
  return output;
}

function imageWindows(width, height) {
  const window = (x0, y0) => [x0, y0, Math.min(width, x0 + 64), Math.min(height, y0 + 64)];
  return {
    topLeft: window(0, 0),
    center: window(Math.max(0, Math.floor(width / 2) - 32), Math.max(0, Math.floor(height / 2) - 32)),
    bottomRight: window(Math.max(0, width - 64), Math.max(0, height - 64)),
  };
}

async function rasterCases(image) {
  const width = image.getWidth();
  const height = image.getHeight();
  const samplesPerPixel = image.getSamplesPerPixel();
  const windows = imageWindows(width, height);
  const samples = samplesPerPixel === 1 ? [0] : [samplesPerPixel - 1, 0];
  const sampleCount = width * height * samplesPerPixel;
  const full = sampleCount <= FULL_SAMPLE_LIMIT ? {
    bands: await capture(async () => rasterSummary(await image.readRasters())),
    interleaved: await capture(async () => rasterSummary(
      await image.readRasters({ interleave: true }),
    )),
  } : {
    classification: 'sampledLargeImage',
    sampleCount,
  };
  return {
    full,
    topLeftBands: await capture(async () => rasterSummary(
      await image.readRasters({ window: windows.topLeft }),
    )),
    topLeftInterleaved: await capture(async () => rasterSummary(
      await image.readRasters({ window: windows.topLeft, interleave: true }),
    )),
    selectedCenterBands: await capture(async () => rasterSummary(
      await image.readRasters({ window: windows.center, samples }),
    )),
    selectedCenterInterleaved: await capture(async () => rasterSummary(
      await image.readRasters({ window: windows.center, samples, interleave: true }),
    )),
    nearestBottomRight: await capture(async () => rasterSummary(
      await image.readRasters({
        window: windows.bottomRight,
        samples,
        interleave: true,
        width: 17,
        height: 13,
        resampleMethod: 'nearest',
      }),
    )),
    bilinearBottomRight: await capture(async () => rasterSummary(
      await image.readRasters({
        window: windows.bottomRight,
        samples,
        interleave: true,
        width: 17,
        height: 13,
        resampleMethod: 'bilinear',
      }),
    )),
    outOfBoundsFill: await capture(async () => rasterSummary(
      await image.readRasters({
        window: [-2, -3, Math.min(width, 6), Math.min(height, 5)],
        samples: [0],
        interleave: true,
        fillValue: 17,
      }),
    )),
  };
}

async function blockCases(image) {
  const directory = image.getFileDirectory();
  const compressionId = await directory.loadValue('Compression');
  const decoderParameters = await compression.getDecoderParameters(compressionId, directory);
  const decoder = await geotiff.getDecoder(compressionId, decoderParameters);
  const columns = Math.max(1, Math.ceil(image.getWidth() / image.getTileWidth()));
  const rows = Math.max(1, Math.ceil(image.getHeight() / image.getTileHeight()));
  const coordinates = [
    [0, 0],
    [Math.floor((columns - 1) / 2), Math.floor((rows - 1) / 2)],
    [columns - 1, rows - 1],
  ];
  const samples = image.getSamplesPerPixel() === 1 ? [0] : [0, image.getSamplesPerPixel() - 1];
  const unique = new Set();
  const output = [];
  for (const [x, y] of coordinates) {
    for (const sample of samples) {
      const key = `${x}/${y}/${sample}`;
      if (unique.has(key)) continue;
      unique.add(key);
      output.push(await capture(async () => {
        const block = await image.getTileOrStrip(x, y, sample, decoder);
        return {
          x: block.x,
          y: block.y,
          sample: block.sample,
          data: typedSummary(new Uint8Array(block.data)),
        };
      }));
    }
  }
  return output;
}

async function rgbCases(image, directory) {
  const window = imageWindows(image.getWidth(), image.getHeight()).topLeft;
  const output = {
    interleaved: await capture(async () => rasterSummary(
      await image.readRGB({ window, interleave: true }),
    )),
    bands: await capture(async () => rasterSummary(await image.readRGB({ window }))),
  };
  output.alpha = directory.hasTag('ExtraSamples')
    ? await capture(async () => rasterSummary(
      await image.readRGB({ window, interleave: true, enableAlpha: true }),
    ))
    : { classification: 'notApplicableNoExtraSamples' };
  return output;
}

async function imageSummary(image, index) {
  const directory = image.getFileDirectory();
  const samples = image.getSamplesPerPixel();
  const bits = [];
  const formats = [];
  for (let sample = 0; sample < samples; sample += 1) {
    bits.push(image.getBitsPerSample(sample));
    formats.push(image.getSampleFormat(sample));
  }
  return {
    index,
    metadata: {
      width: image.getWidth(),
      height: image.getHeight(),
      samplesPerPixel: samples,
      tiled: image.isTiled,
      planarConfiguration: image.planarConfiguration,
      tileWidth: image.getTileWidth(),
      tileHeight: image.getTileHeight(),
      bits,
      formats,
      geoKeys: await capture(() => image.getGeoKeys()),
      tiePoints: await capture(() => image.getTiePoints()),
      gdalMetadata: await capture(() => image.getGDALMetadata()),
      gdalNoData: normalize(image.getGDALNoData()),
      origin: await capture(() => image.getOrigin()),
      resolution: await capture(() => image.getResolution()),
      pixelIsArea: image.pixelIsArea(),
      boundingBox: await capture(() => image.getBoundingBox(false)),
      tilegridBoundingBox: await capture(() => image.getBoundingBox(true)),
      directory: await directorySummary(directory),
    },
    rasters: await rasterCases(image),
    blocks: await blockCases(image),
    rgb: await rgbCases(image, directory),
  };
}

async function fileSummary(name) {
  const filePath = path.join(filesRoot, name);
  const bytes = await fs.readFile(filePath);
  const fileFacts = {
    byteLength: bytes.byteLength,
    sha256: crypto.createHash('sha256').update(bytes).digest('hex'),
  };
  let file;
  try {
    file = await geotiff.fromFile(filePath);
  } catch {
    return { file: fileFacts, open: { error: true } };
  }
  try {
    const imageCount = await file.getImageCount();
    const images = [];
    for (let index = 0; index < imageCount; index += 1) {
      images.push(await imageSummary(await file.getImage(index), index));
    }
    const first = await file.getImage(0);
    return {
      file: fileFacts,
      open: {
        ok: {
          imageCount,
          bigTiff: file.bigTiff,
          littleEndian: file.littleEndian,
          ghostValues: await capture(() => file.getGhostValues()),
          bestFit: await capture(async () => rasterSummary(await file.readRasters({
            window: [0, 0, Math.min(64, first.getWidth()), Math.min(64, first.getHeight())],
            width: Math.min(31, first.getWidth()),
            height: Math.min(29, first.getHeight()),
            interleave: true,
          }))),
          images,
        },
      },
    };
  } finally {
    await file.close();
  }
}

const names = (await fs.readdir(filesRoot))
  .filter((name) => /\.tiff?$/i.test(name))
  .sort();
const files = {};
for (const name of names) {
  files[name] = await fileSummary(name);
}

process.stdout.write(`${JSON.stringify({
  reference: { version: packageJson.version },
  policy: { fullSampleLimit: FULL_SAMPLE_LIMIT },
  names,
  files,
})}\n`);
