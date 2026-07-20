import crypto from 'node:crypto';
import fs from 'node:fs/promises';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [jsRootArg, fixtureRootArg] = process.argv.slice(2);
if (!jsRootArg || !fixtureRootArg) {
  throw new Error('usage: node js_api_oracle.mjs <geotiff.js-root> <fixture-root>');
}

const jsRoot = path.resolve(jsRootArg);
const fixtureRoot = path.resolve(fixtureRootArg);
const moduleUrl = (relative) => pathToFileURL(path.join(jsRoot, 'dist-module', relative));
const geotiff = await import(moduleUrl('geotiff.js'));
const compression = await import(moduleUrl('compression/index.js'));
const { writeGeotiff } = await import(moduleUrl('geotiffwriter.js'));
const { makeBufferSource } = await import(moduleUrl('source/arraybuffer.js'));
const { BaseSource } = await import(moduleUrl('source/basesource.js'));
const { BlockedSource } = await import(moduleUrl('source/blockedsource.js'));
const packageJson = JSON.parse(await fs.readFile(path.join(jsRoot, 'package.json'), 'utf8'));

const mainPath = path.join(fixtureRoot, 'tiled-gray-i1.tif');
const overviewPath = path.join(fixtureRoot, 'minisblack-1c-8b.tiff');
const mainBytes = await fs.readFile(mainPath);
const overviewBytes = await fs.readFile(overviewPath);

function exactArrayBuffer(bytes) {
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
}

function normalize(value) {
  if (value === undefined) return { $undefined: true };
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return value;
  if (typeof value === 'number') {
    if (Number.isNaN(value)) return { $number: 'NaN' };
    if (value === Infinity) return { $number: 'Infinity' };
    if (value === -Infinity) return { $number: '-Infinity' };
    if (Object.is(value, -0)) return { $number: '-0' };
    return value;
  }
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

function typedBytes(array) {
  const name = array.constructor.name;
  const bytes = new Uint8Array(array.length * array.BYTES_PER_ELEMENT);
  const view = new DataView(bytes.buffer);
  for (let index = 0; index < array.length; index += 1) {
    const offset = index * array.BYTES_PER_ELEMENT;
    switch (name) {
      case 'Int8Array': view.setInt8(offset, array[index]); break;
      case 'Uint8Array':
      case 'Uint8ClampedArray': view.setUint8(offset, array[index]); break;
      case 'Int16Array': view.setInt16(offset, array[index], true); break;
      case 'Uint16Array': view.setUint16(offset, array[index], true); break;
      case 'Int32Array': view.setInt32(offset, array[index], true); break;
      case 'Uint32Array': view.setUint32(offset, array[index], true); break;
      case 'BigInt64Array': view.setBigInt64(offset, array[index], true); break;
      case 'BigUint64Array': view.setBigUint64(offset, array[index], true); break;
      case 'Float32Array': view.setFloat32(offset, array[index], true); break;
      case 'Float64Array': view.setFloat64(offset, array[index], true); break;
      default: throw new Error(`unsupported typed array ${name}`);
    }
  }
  return bytes;
}

function bytesSummary(bytes) {
  const view = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  return {
    length: view.byteLength,
    sha256: crypto.createHash('sha256').update(view).digest('hex'),
  };
}

function rasterSummary(raster) {
  return {
    type: raster.constructor.name,
    width: raster.width,
    height: raster.height,
    length: raster.length,
    ...bytesSummary(typedBytes(raster)),
  };
}

async function summarizeSingle(file) {
  const count = await file.getImageCount();
  const image = await file.getImage(0);
  const raster = await image.readRasters({ interleave: true });
  const slice = await file.getSlice(0, 8);
  const directory = await file.requestIFD(0);
  return {
    imageCount: count,
    width: image.getWidth(),
    height: image.getHeight(),
    samplesPerPixel: image.getSamplesPerPixel(),
    raster: rasterSummary(raster),
    slice: Array.from(new Uint8Array(slice.buffer)),
    sliceOffset: slice.sliceOffset,
    requestIfdWidth: directory.getValue('ImageWidth'),
    ghostValues: await file.getGhostValues(),
  };
}

// Node has Blob but intentionally does not expose the browser FileReader API.
// This minimal standards-shaped implementation lets the browser-only source
// adapter itself run unchanged in the live reference oracle.
class OracleFileReader {
  constructor() {
    this.result = null;
    this.aborted = false;
  }

  readAsArrayBuffer(blob) {
    blob.arrayBuffer().then((result) => {
      if (!this.aborted) {
        this.result = result;
        this.onload?.();
      }
    }).catch((error) => this.onerror?.(error));
  }

  abort() {
    this.aborted = true;
    this.onabort?.(new Error('aborted'));
  }
}
globalThis.FileReader = OracleFileReader;

class MemoryResponse extends geotiff.BaseResponse {
  constructor(status, headers, data) {
    super();
    this.statusCode = status;
    this.headers = headers;
    this.data = data;
  }

  get status() { return this.statusCode; }

  getHeader(name) { return this.headers[name.toLowerCase()]; }

  async getData() { return this.data; }
}

class MemoryClient extends geotiff.BaseClient {
  constructor(bytes, delay = 0) {
    super('memory://oracle');
    this.bytes = bytes;
    this.delay = delay;
    this.requests = [];
  }

  async request({ headers = {}, signal } = {}) {
    const range = headers.Range || headers.range;
    this.requests.push(range);
    if (this.delay) {
      await new Promise((resolve, reject) => {
        const timer = setTimeout(resolve, this.delay);
        signal?.addEventListener('abort', () => {
          clearTimeout(timer);
          const error = new Error('Request aborted');
          error.name = 'AbortError';
          reject(error);
        }, { once: true });
      });
    }
    if (signal?.aborted) {
      const error = new Error('Request aborted');
      error.name = 'AbortError';
      throw error;
    }
    const match = /^bytes=(\d+)-(\d+)$/.exec(range || '');
    if (!match) throw new Error(`unexpected Range header: ${range}`);
    const start = Number(match[1]);
    const end = Math.min(Number(match[2]), this.bytes.byteLength - 1);
    const data = this.bytes.slice(start, end + 1);
    return new MemoryResponse(206, {
      'content-range': `bytes ${start}-${end}/${this.bytes.byteLength}`,
      'content-type': 'application/octet-stream',
    }, exactArrayBuffer(data));
  }
}

function parseRange(raw, size) {
  const match = /^bytes=(\d+)-(\d+)$/.exec(raw || '');
  if (!match) return null;
  const start = Number(match[1]);
  if (start >= size) return { start, end: size - 1, unsatisfied: true };
  return { start, end: Math.min(Number(match[2]), size - 1), unsatisfied: false };
}

function withCompression(bytes, compressionId) {
  const output = new Uint8Array(bytes);
  const view = new DataView(output.buffer);
  const littleEndian = view.getUint16(0, false) === 0x4949;
  const ifdOffset = view.getUint32(4, littleEndian);
  const count = view.getUint16(ifdOffset, littleEndian);
  for (let index = 0; index < count; index += 1) {
    const entry = ifdOffset + 2 + (index * 12);
    if (view.getUint16(entry, littleEndian) === 259) {
      view.setUint16(entry + 8, compressionId, littleEndian);
      return output;
    }
  }
  throw new Error('Compression tag not found');
}

async function startServer() {
  const requests = [];
  const server = http.createServer((request, response) => {
    const url = new URL(request.url, 'http://127.0.0.1');
    const bytes = url.pathname.includes('overview') ? overviewBytes : mainBytes;
    requests.push({
      path: url.pathname,
      range: request.headers.range,
      oracleHeader: request.headers['x-oracle'],
    });
    if (url.pathname.includes('full')) {
      response.writeHead(200, {
        'Content-Type': 'application/octet-stream',
        'Content-Length': bytes.byteLength,
      });
      response.end(bytes);
      return;
    }
    const range = parseRange(request.headers.range, bytes.byteLength);
    if (!range || range.unsatisfied) {
      response.writeHead(416, { 'Content-Range': `bytes */${bytes.byteLength}` });
      response.end();
      return;
    }
    const body = bytes.subarray(range.start, range.end + 1);
    response.writeHead(206, {
      'Content-Type': 'application/octet-stream',
      'Content-Range': `bytes ${range.start}-${range.end}/${bytes.byteLength}`,
      'Content-Length': body.byteLength,
    });
    response.end(body);
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const address = server.address();
  return {
    server,
    requests,
    baseUrl: `http://127.0.0.1:${address.port}`,
  };
}

async function factoryCases() {
  const arrayFile = await geotiff.fromArrayBuffer(exactArrayBuffer(mainBytes));
  const arrayBuffer = await summarizeSingle(arrayFile);
  await arrayFile.close();

  const fileFile = await geotiff.fromFile(mainPath);
  const file = await summarizeSingle(fileFile);
  const fileClose = normalize(await fileFile.close());

  const blobFile = await geotiff.fromBlob(new Blob([mainBytes]));
  const blob = await summarizeSingle(blobFile);
  await blobFile.close();

  const memoryClient = new MemoryClient(mainBytes);
  const customFile = await geotiff.fromCustomClient(memoryClient, { blockSize: undefined });
  const customClient = await summarizeSingle(customFile);
  await customFile.close();

  const staticFile = await geotiff.GeoTIFF.fromSource(
    makeBufferSource(exactArrayBuffer(mainBytes)),
    { cache: true },
  );
  const fromSource = await summarizeSingle(staticFile);
  await staticFile.close();

  const { server, requests, baseUrl } = await startServer();
  let url;
  let fullAllowed;
  let fullRejected;
  let multi;
  try {
    const remote = await geotiff.fromUrl(`${baseUrl}/main`, {
      blockSize: undefined,
      headers: { 'X-Oracle': 'present' },
    });
    url = await summarizeSingle(remote);
    await remote.close();

    fullRejected = await capture(() => geotiff.fromUrl(`${baseUrl}/full`));

    const allowed = await geotiff.fromUrl(`${baseUrl}/full`, {
      allowFullFile: true,
    });
    fullAllowed = await summarizeSingle(allowed);
    await allowed.close();

    const multiFile = await geotiff.fromUrls(
      `${baseUrl}/main-multi`,
      [`${baseUrl}/overview`],
      { blockSize: undefined },
    );
    const directories = await multiFile.parseFileDirectoriesPerFile();
    const image0 = await multiFile.getImage(0);
    const image1 = await multiFile.getImage(1);
    multi = {
      imageCount: await multiFile.getImageCount(),
      directoryWidths: directories.map((directory) => directory.getValue('ImageWidth')),
      imageWidths: [image0.getWidth(), image1.getWidth()],
      imageHeights: [image0.getHeight(), image1.getHeight()],
      secondRaster: rasterSummary(await image1.readRasters({ interleave: true })),
    };
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }

  return {
    arrayBuffer,
    file,
    fileClose,
    blob,
    customClient,
    customClientRequests: memoryClient.requests,
    fromSource,
    url,
    fullRejected,
    fullAllowed,
    multi,
    httpRequestFacts: {
      customHeaderSeen: requests.some((request) => request.oracleHeader === 'present'),
      allReadsRangedExceptFullEndpoint: requests
        .filter((request) => !request.path.includes('full'))
        .every((request) => typeof request.range === 'string'),
    },
  };
}

async function sourceCases() {
  class RecordingSource extends BaseSource {
    constructor(bytes) {
      super();
      this.bytes = bytes;
      this.requests = [];
    }

    get fileSize() { return this.bytes.byteLength; }

    async fetchSlice(slice, signal) {
      if (signal?.aborted) {
        const error = new Error('Request aborted');
        error.name = 'AbortError';
        throw error;
      }
      this.requests.push([slice.offset, slice.length]);
      return {
        data: exactArrayBuffer(this.bytes.slice(slice.offset, slice.offset + slice.length)),
        offset: slice.offset,
        length: slice.length,
      };
    }
  }

  const inner = new RecordingSource(Buffer.from('0123456789abcdef'));
  const blocked = new BlockedSource(inner, { blockSize: 4, cacheSize: 8 });
  const first = await blocked.fetch([{ offset: 3, length: 7 }]);
  const second = await blocked.fetch([{ offset: 4, length: 2 }]);
  await blocked.close();
  return {
    first: Array.from(new Uint8Array(first[0])),
    second: Array.from(new Uint8Array(second[0])),
    requests: inner.requests,
    fileSize: blocked.fileSize,
  };
}

async function decoderAndCacheCases() {
  let decodeCount = 0;
  class CountingRawDecoder extends geotiff.BaseDecoder {
    decodeBlock(buffer) {
      decodeCount += 1;
      return buffer;
    }
  }
  geotiff.addDecoder(1, async () => CountingRawDecoder, undefined, false);

  async function cacheRun(cache) {
    decodeCount = 0;
    const file = await geotiff.GeoTIFF.fromSource(
      makeBufferSource(exactArrayBuffer(mainBytes)),
      { cache },
    );
    const image = await file.getImage(0);
    const first = await image.readRasters({ interleave: true });
    const firstCount = decodeCount;
    const second = await image.readRasters({ interleave: true });
    const secondCount = decodeCount;
    await file.close();
    return {
      firstCount,
      secondCount,
      first: rasterSummary(first),
      second: rasterSummary(second),
    };
  }

  class ParameterDecoder extends geotiff.BaseDecoder {
    decodeBlock(buffer) {
      const values = new Uint8Array(buffer.slice(0));
      values[0] = (values[0] + this.parameters.marker) & 0xff;
      return values.buffer;
    }
  }
  geotiff.addDecoder(
    65000,
    async () => ParameterDecoder,
    async (directory) => ({ marker: await directory.loadValue('ImageWidth') }),
    false,
  );
  const parameters = await compression.getDecoderParameters(65000, {
    loadValue: async (name) => (name === 'ImageWidth' ? 37 : undefined),
  });
  const decoder = await geotiff.getDecoder(65000, parameters);
  const customOutput = Array.from(new Uint8Array(await decoder.decode(new Uint8Array([1, 2, 3]).buffer)));
  const customFile = await geotiff.fromArrayBuffer(exactArrayBuffer(withCompression(mainBytes, 65000)));
  const customImage = await customFile.getImage(0);
  const customRaster = rasterSummary(await customImage.readRasters({ interleave: true }));
  await customFile.close();

  const inlinePool = new geotiff.Pool(0);
  const inlineDecoder = inlinePool.bindParameters(1, {
    tileWidth: 4,
    tileHeight: 1,
    predictor: 1,
    bitsPerSample: [8],
    planarConfiguration: 1,
  });
  const inlineOutput = Array.from(new Uint8Array(await inlineDecoder.decode(
    new Uint8Array([9, 8, 7, 6]).buffer,
  )));
  const firstDestroy = normalize(await inlinePool.destroy());
  const secondDestroy = normalize(await inlinePool.destroy());

  return {
    uncached: await cacheRun(false),
    cached: await cacheRun(true),
    custom: { parameters, output: customOutput, raster: customRaster },
    inlinePool0: { output: inlineOutput, firstDestroy, secondDestroy },
  };
}

async function cancellationCases() {
  const pre = new AbortController();
  pre.abort();
  const preCancelled = await capture(() => geotiff.fromArrayBuffer(
    exactArrayBuffer(mainBytes),
    pre.signal,
  ));

  const client = new MemoryClient(mainBytes, 100);
  const controller = new AbortController();
  const pending = geotiff.fromCustomClient(
    client,
    { blockSize: undefined },
    controller.signal,
  );
  setTimeout(() => controller.abort(), 5);
  const inFlightCancelled = await capture(() => pending);
  return {
    preCancelled,
    inFlightCancelled,
    inFlightRequests: client.requests.length,
  };
}

function arrangeTiledDataInterleaved(
  original,
  width,
  height,
  tileWidth,
  tileHeight,
  samplesPerPixel,
  Ctor = Uint8Array,
) {
  const output = [];
  for (let tileY = 0; tileY < Math.ceil(height / tileHeight); tileY += 1) {
    for (let tileX = 0; tileX < Math.ceil(width / tileWidth); tileX += 1) {
      for (let y = 0; y < tileHeight; y += 1) {
        for (let x = 0; x < tileWidth; x += 1) {
          const sourceX = (tileX * tileWidth) + x;
          const sourceY = (tileY * tileHeight) + y;
          for (let sample = 0; sample < samplesPerPixel; sample += 1) {
            output.push(sourceX < width && sourceY < height
              ? original[(((sourceY * width) + sourceX) * samplesPerPixel) + sample]
              : 0);
          }
        }
      }
    }
  }
  return new Ctor(output);
}

function arrangeTiledDataPlanar(bands, width, height, tileWidth, tileHeight) {
  const output = [];
  for (const band of bands) {
    for (let tileY = 0; tileY < Math.ceil(height / tileHeight); tileY += 1) {
      for (let tileX = 0; tileX < Math.ceil(width / tileWidth); tileX += 1) {
        for (let y = 0; y < tileHeight; y += 1) {
          for (let x = 0; x < tileWidth; x += 1) {
            const sourceX = (tileX * tileWidth) + x;
            const sourceY = (tileY * tileHeight) + y;
            output.push(sourceX < width && sourceY < height ? band[sourceY][sourceX] : 0);
          }
        }
      }
    }
  }
  return new Uint8Array(output);
}

function writerInputs() {
  const nested = [
    [[1, 2], [3, 4]],
    [[5, 6], [7, 8]],
    [[9, 10], [11, 12]],
  ];
  const red = [[255, 255, 255], [1, 1, 1], [1, 1, 1]];
  const green = [[2, 2, 2], [255, 255, 255], [2, 2, 2]];
  const blue = [[3, 3, 3], [3, 3, 3], [255, 255, 255]];
  const interleaved = red.flatMap((row, y) => row.flatMap((value, x) => [
    value, green[y][x], blue[y][x],
  ]));
  const floatInterleaved = interleaved.map((value, index) => value + ((index % 3) + 1) / 10);
  return {
    uint8: [new Uint8Array([1, 2, 3, 4]), { width: 2, height: 2 }],
    int8: [new Int8Array([-128, -2, 3, 127]), { width: 2, height: 2 }],
    uint16: [new Uint16Array([0, 2, 65534, 65535]), { width: 2, height: 2 }],
    int16: [new Int16Array([-32768, -2, 3, 32767]), { width: 2, height: 2 }],
    uint32: [new Uint32Array([0, 2, 4294967294, 4294967295]), { width: 2, height: 2 }],
    int32: [new Int32Array([-2147483648, -2, 3, 2147483647]), { width: 2, height: 2 }],
    float32: [new Float32Array([-1.5, 0, 2.25, 8.5]), { width: 2, height: 2 }],
    float64: [new Float64Array([-1.5, 0, Math.PI, 8.5]), { width: 2, height: 2 }],
    flatNumbers: [[1, 2, 3, 4], { width: 2, height: 2 }],
    nested: [nested, {}],
    richMetadata: [new Uint16Array([1, 2, 3, 4]), {
      width: 2,
      height: 2,
      GDAL_NODATA: '-9999\0',
      Orientation: 3,
      GeographicTypeGeoKey: 4326,
      GeogCitationGeoKey: 'X',
      GTRasterTypeGeoKey: 1,
    }],
    tiled: [new Uint8Array(Array.from({ length: 27 }, (_, index) => index)), {
      width: 3,
      height: 3,
      SamplesPerPixel: 3,
      TileWidth: 3,
      TileLength: 3,
      TileByteCounts: [27],
    }],
    tiledInterleaved: [arrangeTiledDataInterleaved(interleaved, 3, 3, 2, 2, 3), {
      width: 3,
      height: 3,
      SamplesPerPixel: 3,
      TileWidth: 2,
      TileLength: 2,
      TileByteCounts: [12, 12, 12, 12],
    }],
    tiledPlanar: [arrangeTiledDataPlanar([red, green, blue], 3, 3, 2, 2), {
      width: 3,
      height: 3,
      SamplesPerPixel: 3,
      PlanarConfiguration: 2,
      TileWidth: 2,
      TileLength: 2,
      TileByteCounts: Array(12).fill(4),
    }],
    tiledFloat64: [arrangeTiledDataInterleaved(
      floatInterleaved,
      3,
      3,
      2,
      2,
      3,
      Float64Array,
    ), {
      width: 3,
      height: 3,
      SamplesPerPixel: 3,
      TileWidth: 2,
      TileLength: 2,
      TileByteCounts: [96, 96, 96, 96],
    }],
    zeroTile: [new Uint8Array([9, 8, 7, 6]), {
      width: 2,
      height: 2,
      SamplesPerPixel: 1,
      TileWidth: 2,
      TileLength: 2,
      TileByteCounts: [0],
    }],
    zeroTileNoData: [new Uint8Array([9, 8, 7, 6]), {
      width: 2,
      height: 2,
      SamplesPerPixel: 1,
      TileWidth: 2,
      TileLength: 2,
      TileByteCounts: [0],
      GDAL_NODATA: '7\0',
    }],
    multiStrip: [new Uint8Array([1, 2, 3, 4, 5, 6]), {
      width: 3,
      height: 2,
      RowsPerStrip: 1,
      StripByteCounts: [3, 3],
    }],
  };
}

function writerCases() {
  const cases = writerInputs();
  return Object.fromEntries(Object.entries(cases).map(([name, [values, metadata]]) => {
    const root = new Uint8Array(geotiff.writeArrayBuffer(values, structuredClone(metadata)));
    const direct = new Uint8Array(writeGeotiff(values, structuredClone(metadata)));
    return [name, {
      root: bytesSummary(root),
      direct: bytesSummary(direct),
      header: Array.from(root.slice(0, 8)),
    }];
  }));
}

async function writerReadbackCases() {
  const output = {};
  for (const [name, [values, metadata]] of Object.entries(writerInputs())) {
    const bytes = geotiff.writeArrayBuffer(values, structuredClone(metadata));
    const file = await geotiff.fromArrayBuffer(bytes);
    try {
      const image = await file.getImage(0);
      output[name] = rasterSummary(await image.readRasters({ interleave: true }));
    } finally {
      await file.close();
    }
  }
  return output;
}

function withShortTag(bytes, tag, value) {
  const output = new Uint8Array(bytes);
  const view = new DataView(output.buffer);
  const littleEndian = view.getUint16(0, false) === 0x4949;
  const ifdOffset = view.getUint32(4, littleEndian);
  const count = view.getUint16(ifdOffset, littleEndian);
  for (let index = 0; index < count; index += 1) {
    const entry = ifdOffset + 2 + (index * 12);
    if (view.getUint16(entry, littleEndian) === tag) {
      view.setUint16(entry + 8, value, littleEndian);
      return output.buffer;
    }
  }
  throw new Error(`SHORT tag ${tag} not found`);
}

async function rgbDispatchCases() {
  const rgba = [10, 20, 30, 40, 200, 150, 100, 128];
  const cases = {
    whiteIsZero: {
      bytes: withShortTag(
        geotiff.writeArrayBuffer(new Uint8Array([0, 64, 255, 128]), { width: 2, height: 2 }),
        262,
        0,
      ),
      options: { interleave: true },
    },
    cmyk: {
      bytes: geotiff.writeArrayBuffer(new Uint8Array([
        0, 0, 0, 0,
        255, 0, 0, 0,
      ]), {
        width: 2,
        height: 1,
        SamplesPerPixel: 4,
        PhotometricInterpretation: 5,
      }),
      options: { interleave: true },
    },
    cielab: {
      bytes: geotiff.writeArrayBuffer(new Uint8Array([
        100, 128, 128,
        200, 100, 150,
      ]), {
        width: 2,
        height: 1,
        SamplesPerPixel: 3,
        PhotometricInterpretation: 8,
      }),
      options: { interleave: true },
    },
    rgbaWithoutAlpha: {
      bytes: geotiff.writeArrayBuffer(new Uint8Array(rgba), {
        width: 2,
        height: 1,
        SamplesPerPixel: 4,
        PhotometricInterpretation: 2,
        ExtraSamples: [2],
      }),
      options: { interleave: true, enableAlpha: false },
    },
    rgbaWithAlpha: {
      bytes: geotiff.writeArrayBuffer(new Uint8Array(rgba), {
        width: 2,
        height: 1,
        SamplesPerPixel: 4,
        PhotometricInterpretation: 2,
        ExtraSamples: [2],
      }),
      options: { interleave: true, enableAlpha: true },
    },
  };
  const output = {};
  for (const [name, testCase] of Object.entries(cases)) {
    const file = await geotiff.fromArrayBuffer(testCase.bytes);
    try {
      const image = await file.getImage(0);
      output[name] = rasterSummary(await image.readRGB(testCase.options));
    } finally {
      await file.close();
    }
  }
  return output;
}

const output = {
  reference: { version: packageJson.version },
  factories: await factoryCases(),
  sources: await sourceCases(),
  decoderAndCache: await decoderAndCacheCases(),
  cancellation: await cancellationCases(),
  writer: writerCases(),
  writerReadbacks: await writerReadbackCases(),
  rgbDispatch: await rgbDispatchCases(),
};

process.stdout.write(`${JSON.stringify(normalize(output))}\n`);
