import path from 'node:path';
import fs from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

const [jsRootArg] = process.argv.slice(2);
if (!jsRootArg) throw new Error('usage: node js_helpers_oracle.mjs <geotiff.js-root>');
const jsRoot = path.resolve(jsRootArg);
const moduleUrl = (relative) => pathToFileURL(path.join(jsRoot, 'dist-module', relative));

const packageJson = JSON.parse(await fs.readFile(path.join(jsRoot, 'package.json'), 'utf8'));
const DataView64 = (await import(moduleUrl('dataview64.js'))).default;
const DataSlice = (await import(moduleUrl('dataslice.js'))).default;
const resample = await import(moduleUrl('resample.js'));
const rgb = await import(moduleUrl('rgb.js'));
const predictor = await import(moduleUrl('predictor.js'));
const utils = await import(moduleUrl('utils.js'));
const globals = await import(moduleUrl('globals.js'));
const logging = await import(moduleUrl('logging.js'));
const geotiff = await import(moduleUrl('geotiff.js'));
const httpUtils = await import(moduleUrl('source/httputils.js'));
const RawDecoder = (await import(moduleUrl('compression/raw.js'))).default;
const PackbitsDecoder = (await import(moduleUrl('compression/packbits.js'))).default;
const LzwDecoder = (await import(moduleUrl('compression/lzw.js'))).default;
const BaseDecoder = (await import(moduleUrl('compression/basedecoder.js'))).default;

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
  if (typeof value === 'object') {
    return Object.fromEntries(Object.keys(value).sort().map((key) => [key, normalize(value[key])]));
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
      default: throw new Error(`unsupported typed array: ${name}`);
    }
  }
  return Array.from(bytes);
}

function typed(array) {
  return { type: array.constructor.name, bytes: typedBytes(array) };
}

function withDimensions(array, width, height) {
  array.width = width;
  array.height = height;
  return array;
}

async function capture(operation) {
  try {
    return { ok: normalize(await operation()) };
  } catch (error) {
    return { error: { name: error?.constructor?.name || 'Error', message: String(error?.message ?? error) } };
  }
}

function captureSync(operation) {
  try {
    return { ok: normalize(operation()) };
  } catch (error) {
    return { error: { name: error?.constructor?.name || 'Error', message: String(error?.message ?? error) } };
  }
}

function dataViewCases() {
  const buffer = new ArrayBuffer(48);
  const view = new DataView(buffer);
  view.setBigUint64(0, 9007199254740991n, true);
  view.setBigInt64(8, -123456789n, false);
  view.setUint8(16, 250);
  view.setUint16(18, 0xabcd, true);
  view.setInt16(20, -1234, false);
  view.setUint32(22, 0xdeadbeef, true);
  view.setInt32(26, -123456, false);
  view.setUint16(30, 0x3e00, true);
  view.setFloat32(32, -12.5, true);
  view.setFloat64(36, Math.PI, false);
  const dataView = new DataView64(buffer);
  const sliceBuffer = buffer.slice(0);
  const sliceView = new DataView(sliceBuffer);
  sliceView.setBigInt64(8, -987654321n, true);
  sliceView.setFloat64(36, Math.PI, true);
  const slice = new DataSlice(sliceBuffer, 100, true, true);
  const zeroOffsetSlice = new DataSlice(sliceBuffer, 0, true, true);
  return {
    dataView64: {
      buffer: normalize(dataView.buffer),
      uint64Le: dataView.getUint64(0, true),
      int64Be: dataView.getInt64(8, false),
      uint8: dataView.getUint8(16),
      int8: dataView.getInt8(16),
      uint16Le: dataView.getUint16(18, true),
      int16Be: dataView.getInt16(20, false),
      uint32Le: dataView.getUint32(22, true),
      int32Be: dataView.getInt32(26, false),
      float16Le: dataView.getFloat16(30, true),
      float32Le: dataView.getFloat32(32, true),
      float64Be: dataView.getFloat64(36, false),
    },
    dataSlice: {
      sliceOffset: slice.sliceOffset,
      sliceTop: slice.sliceTop,
      littleEndian: slice.littleEndian,
      bigTiff: slice.bigTiff,
      coversWhole: slice.covers(100, 48),
      coversInner: slice.covers(108, 8),
      coversBefore: slice.covers(99, 1),
      uint64Le: slice.readUint64(100),
      int64Le: zeroOffsetSlice.readInt64(8),
      int64NonZeroOffset: captureSync(() => slice.readInt64(108)),
      uint8: slice.readUint8(116),
      int8: slice.readInt8(116),
      uint16Le: slice.readUint16(118),
      int16Le: slice.readInt16(120),
      uint32Le: slice.readUint32(122),
      int32Le: slice.readInt32(126),
      float32Le: slice.readFloat32(132),
      float64Le: slice.readFloat64(136),
      offset: slice.readOffset(100),
    },
  };
}

function resampleCases() {
  const bands = [
    new Uint16Array([1, 2, 3, 4, 5, 6]),
    new Int16Array([-30, -20, -10, 10, 20, 30]),
    new Float32Array([0.25, 1.5, -2.25, 3.75, 4.5, 9.25]),
  ];
  const interleaved = new Uint16Array([
    1, 101, 2, 102, 3, 103,
    4, 104, 5, 105, 6, 106,
  ]);
  const describe = (arrays) => arrays.map(typed);
  return {
    nearest: describe(resample.resampleNearest(bands, 3, 2, 5, 4)),
    bilinear: describe(resample.resampleBilinear(bands, 3, 2, 5, 4)),
    dispatchLinear: describe(resample.resample(bands, 3, 2, 5, 4, 'LiNeAr')),
    nearestInterleaved: typed(resample.resampleNearestInterleaved(interleaved, 3, 2, 5, 4, 2)),
    bilinearInterleaved: typed(resample.resampleBilinearInterleaved(interleaved, 3, 2, 5, 4, 2)),
    dispatchInterleaved: typed(resample.resampleInterleaved(interleaved, 3, 2, 5, 4, 2, 'BILINEAR')),
  };
}

function rgbCases() {
  const colorMap = new Uint16Array([
    0, 16384, 32768, 49152, 65535,
    65535, 49152, 32768, 16384, 0,
    0, 8192, 24576, 40960, 65535,
  ]);
  return {
    whiteIsZero: typed(rgb.fromWhiteIsZero(withDimensions(new Uint16Array([0, 1, 2, 3]), 2, 2), 4)),
    blackIsZero: typed(rgb.fromBlackIsZero(withDimensions(new Uint16Array([0, 1, 2, 3]), 2, 2), 4)),
    palette: typed(rgb.fromPalette(withDimensions(new Uint8Array([0, 1, 2, 3, 4]), 5, 1), colorMap)),
    cmyk: typed(rgb.fromCMYK(withDimensions(new Uint8Array([0, 0, 0, 0, 20, 40, 60, 80]), 2, 1))),
    yCbCr: typed(rgb.fromYCbCr(withDimensions(new Uint8Array([0, 128, 128, 255, 128, 128, 100, 10, 240]), 3, 1))),
    cieLab: typed(rgb.fromCIELab(withDimensions(new Uint8Array([0, 128, 128, 128, 0, 0, 255, 127, 255]), 3, 1))),
  };
}

function predictorCases() {
  const apply = (bytes, kind, width, height, bits, planar) => {
    const buffer = new Uint8Array(bytes).buffer;
    predictor.applyPredictor(buffer, kind, width, height, bits, planar);
    return Array.from(new Uint8Array(buffer));
  };
  const p16 = new Uint16Array([1, 2, 3, 10, 20, 30]);
  return {
    none: apply([1, 2, 3, 4], 1, 4, 1, [8], 1),
    horizontal8Chunky: apply([1, 10, 2, 20, 3, 30, 4, 40, 5, 50, 6, 60], 2, 3, 2, [8, 8], 1),
    horizontal16Planar: apply(typedBytes(p16), 2, 3, 2, [16], 2),
    floating32: apply([
      63, 0, 1, 255,
      128, 2, 1, 1,
      0, 0, 0, 0,
      0, 0, 0, 0,
    ], 3, 4, 1, [32], 1),
  };
}

async function utilityCases() {
  const forEachValues = [];
  utils.forEach(new Uint8Array([4, 5, 6]), (value, index) => forEachValues.push([value, index]));
  const nested = [new Uint8Array([1, 2]), [new Int16Array([-3, 4])]];
  return normalize({
    assign: utils.assign({ a: 1, keep: true }, { a: 2, b: 3 }),
    chunk: utils.chunk([1, 2, 3, 4, 5], 3),
    endsWithTrue: utils.endsWith('geotiff.tiff', '.tiff'),
    endsWithFalse: utils.endsWith('tif', '.tiff'),
    forEach: forEachValues,
    invert: utils.invert({ one: 1, two: 2 }),
    range: utils.range(5),
    times: utils.times(4, (index) => index * index),
    toArray: utils.toArray(new Uint16Array([7, 8, 9])),
    recursively: utils.toArrayRecursively(nested),
    contentRanges: [
      utils.parseContentRange('bytes 10-19/100'),
      utils.parseContentRange('items 5-9/*'),
      utils.parseContentRange('bytes */123'),
      utils.parseContentRange(''),
    ],
    zip: utils.zip([1, 2, 3], ['a', 'b']),
    typed: {
      float: utils.isTypedFloatArray(new Float32Array()),
      floatReject: utils.isTypedFloatArray(new Uint32Array()),
      int: utils.isTypedIntArray(new Int16Array()),
      intReject: utils.isTypedIntArray(new BigInt64Array()),
      uint: utils.isTypedUintArray(new Uint8ClampedArray()),
      uintReject: utils.isTypedUintArray(new BigUint64Array()),
    },
    abortError: { name: new utils.AbortError('stop').name, message: new utils.AbortError('stop').message },
    aggregateError: (() => {
      const error = new utils.AggregateError([new Error('a'), new Error('b')], 'many');
      return { name: error.name, message: error.message, errors: error.errors.map((item) => item.message) };
    })(),
    typeMap: Object.keys(utils.typeMap).sort(),
    wait: await capture(() => utils.wait(0)),
  });
}

function globalCases() {
  const sizes = {};
  for (const [name, id] of Object.entries(globals.fieldTypes)) {
    sizes[name] = { id, size: globals.getFieldTypeSize(id) };
  }
  globals.registerTag(65001, 'DifferentialPrivateTag', 'LONG', true, true);
  return normalize({
    exports: Object.keys(globals).sort(),
    sizes,
    imageWidthByName: globals.getTag('ImageWidth'),
    imageWidthById: globals.getTag(256),
    unknownName: globals.resolveTag('NotATiffTag'),
    privateByName: globals.resolveTag('DifferentialPrivateTag'),
    privateDefinition: globals.getTag(65001),
    constants: {
      rgb: globals.photometricInterpretations.RGB,
      alpha: globals.ExtraSamplesValues.Assocalpha,
      lercZstd: globals.LercAddCompression.Zstandard,
      rasterType: globals.geoKeys.GTRasterTypeGeoKey,
    },
  });
}

function httpCases() {
  const multipart = [
    '--oracle',
    'Content-Type: application/octet-stream',
    'Content-Range: bytes 2-4/10',
    '',
    'abc',
    '--oracle',
    'Content-Type: application/octet-stream',
    'Content-Range: bytes 7-8/10',
    '',
    'de',
    '--oracle--',
    '',
  ].join('\r\n');
  const parts = httpUtils.parseByteRanges(new TextEncoder().encode(multipart).buffer, 'oracle');
  return normalize({
    contentTypes: [
      httpUtils.parseContentType(undefined),
      httpUtils.parseContentType('multipart/byteranges; boundary=oracle; charset=utf-8'),
    ],
    contentRanges: [
      httpUtils.parseContentRange(undefined),
      httpUtils.parseContentRange('bytes 10-19/100'),
    ],
    multipart: parts.map((part) => ({
      headers: part.headers,
      data: Array.from(new Uint8Array(part.data)),
      offset: part.offset,
      length: part.length,
      fileSize: part.fileSize,
    })),
  });
}

async function codecCases() {
  const parameters = { tileWidth: 4, tileHeight: 1, predictor: 1, bitsPerSample: [8], planarConfiguration: 1 };
  const raw = new RawDecoder(parameters);
  const packbits = new PackbitsDecoder(parameters);
  const lzw = new LzwDecoder(parameters);
  const rawInput = new Uint8Array([1, 2, 3, 4]);
  const packbitsInput = new Uint8Array([2, 10, 20, 30, 0xfd, 99]);
  const lzwInput = new Uint8Array([32, 144, 96, 68, 34, 20, 22, 2]);
  return {
    rawDecodeBlock: Array.from(new Uint8Array(raw.decodeBlock(rawInput.buffer))),
    rawDecode: Array.from(new Uint8Array(await raw.decode(rawInput.buffer))),
    packbits: Array.from(new Uint8Array(packbits.decodeBlock(packbitsInput.buffer))),
    lzw: Array.from(new Uint8Array(lzw.decodeBlock(lzwInput.buffer))),
    abstractError: await capture(() => new BaseDecoder(parameters).decodeBlock(rawInput.buffer)),
  };
}

function loggingCases() {
  const calls = [];
  const logger = Object.fromEntries([
    'debug', 'log', 'info', 'warn', 'error', 'time', 'timeEnd',
  ].map((method) => [method, (message) => calls.push([method, message])]));
  // Install through the package root and call through the module wrappers,
  // proving both public paths share the same logger binding.
  geotiff.setLogger(logger);
  logging.debug('debug-message');
  logging.log('log-message');
  logging.info('info-message');
  logging.warn('warn-message');
  logging.error('error-message');
  logging.time('timer');
  logging.timeEnd('timer');
  geotiff.setLogger();
  logging.log('not-recorded-after-reset');
  return calls;
}

const output = {
  reference: { version: packageJson.version },
  moduleExports: {
    resample: Object.keys(resample).sort(),
    rgb: Object.keys(rgb).sort(),
    predictor: Object.keys(predictor).sort(),
    utils: Object.keys(utils).sort(),
    httpUtils: Object.keys(httpUtils).sort(),
    logging: Object.keys(logging).sort(),
  },
  ...dataViewCases(),
  resample: resampleCases(),
  rgb: rgbCases(),
  predictor: predictorCases(),
  utils: await utilityCases(),
  globals: globalCases(),
  http: httpCases(),
  codecs: await codecCases(),
  logging: loggingCases(),
};

process.stdout.write(`${JSON.stringify(output)}\n`);
