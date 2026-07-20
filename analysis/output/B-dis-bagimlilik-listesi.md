# B. Dış Bağımlılık Listesi — geotiff.js

**Kapsam:** `geotiff.js` reposunun `src/` dizini (36 dosya).
**Commit:** `8594d1b4bde4072326916185c848e73a9e704850` (2026-05-26). **Analiz tarihi:** 2026-07-19.

## 1. Yöntem

Çıkarım, metin/regex tabanlı satır taraması yerine **AST üzerinden** yapıldı (`analysis/tool/external-deps.mjs`, ts-morph): `ImportDeclaration`, `ExportDeclaration` (re-export), dinamik `import(...)` (`CallExpression` + `ImportKeyword`), ve `require(...)` çağrıları gezildi; her birinden modül belirteci (`from '<paket>'` / argüman string'i) alındı. Bu yaklaşım çok satırlı import'larda da doğru çalışır çünkü satır değil AST düğümü eşleştiriliyor.

Göreli (`./`, `../`) ve kök (`/`) yollar filtrelendi; kalan belirteçler paket kimliğine indirgendi (scoped paketlerde `@scope/name`, aksi halde ilk path segmenti — örn. `xml-utils/get-attribute` → `xml-utils`).

**Yanlış pozitif ele alma:** AST tabanlı yaklaşım, düz yorumlardaki metni (`/** ... import { x } from 'geotiff'; ... */` gibi örnek kod içeren JSDoc açıklama metinleri) zaten hiç görmez — bunlar sözdizimi ağacının parçası değildir. Ancak `checkJs: true` altında JSDoc **tip** açıklamaları (`@import ... from "pkg"` etiketi, `import("pkg").Type` satır-içi tip referansı) gerçek AST düğümleridir ve ayrıca tarandı; bunlar **çalışma zamanı bağımlılığı olarak sayılmadı**, `type-reference` / `jsdoc-@import` etiketiyle ayrı işaretlendi (bu çalıştırmada böyle bir paket-seviyeli tip-only referans bulunmadı — geotiff.js'teki tek `import(...)` tipi kullanımları göreli yollara işaret ediyor, örn. `import('../geotiff.js').TypedArray`). Projenin kendi paket adı (`geotiff`) da hariç tutuldu.

**Manifest karşılaştırması:** `package.json`'daki `dependencies` + `peerDependencies` (peerDependencies boş) "çalışma zamanı" kümesi olarak alındı; `devDependencies` ayrı işaretlendi. Node.js çekirdek modülleri (`node:module` `builtinModules` listesi) manifestte bulunması beklenmediği için ayrı bir sınıfa alındı (aksi halde "hayalet bağımlılık" olarak yanlış işaretlenirlerdi).

## 2. Sonuç tablosu

| Paket | Sınıflandırma | Dosya sayısı | İçe aktarılan isimler |
|---|---|---:|---|
| `@petamoriken/float16` | eşleşti (manifestte + kodda) | 2 | `getFloat16` |
| `lerc` | eşleşti | 1 | `default as Lerc` |
| `pako` | eşleşti | 2 | `inflate` |
| `quick-lru` | eşleşti | 1 | `default as QuickLRU` |
| `web-worker` | eşleşti | 1 | `default as Worker` |
| `xml-utils` | eşleşti | 1 | `findTagsByName`, `getAttribute` (alt-yol importları) |
| `zstddec` | eşleşti | 2 | `ZSTDDecoder` |
| `fs` | Node.js çekirdek modülü (npm paketi değil) | 1 | `default as fs` |
| `http` | Node.js çekirdek modülü | 1 | `default as http` |
| `https` | Node.js çekirdek modülü | 1 | `default as https` |
| `url` | Node.js çekirdek modülü | 1 | `default as urlMod` |
| `parse-headers` | **ölü bağımlılık** (manifestte var, kodda hiç import edilmiyor) | 0 | — |

Üç olası sonuç kategorisinden üçü de gözlemlendi:
- **Birebir örtüşme:** 7 paket (float16, lerc, pako, quick-lru, web-worker, xml-utils, zstddec).
- **Hayalet bağımlılık (kodda import edilip manifestte olmayan):** **bulunamadı.** `fs`/`http`/`https`/`url` ilk bakışta bu kategoriye girecek gibi göründü ama bunlar npm paketi değil Node.js çekirdek modülleri — manifestte bulunmaları zaten beklenmez, dolayısıyla gerçek bir hayalet bağımlılık değiller (bkz. §1, yöntem notu).
- **Ölü bağımlılık (manifestte var, kodda kullanılmıyor):** `parse-headers` — bkz. §3.

## 3. Bulgu: `parse-headers` ölü bağımlılık

`package.json` → `dependencies.parse-headers: "^2.0.2"` bildiriliyor, ancak `src/` içinde hiçbir dosya bu paketi import etmiyor. Doğrulama: `src/source/httputils.js:26` kendi yerel `parseHeaders(text)` fonksiyonunu tanımlıyor ve onu kullanıyor (`httputils.js:134`) — paket muhtemelen bir noktada yerel bir implementasyonla değiştirilmiş ama `package.json`'dan kaldırılmamış. Temizlik adayı.

## 4. Paket bazında rol ve konum

| Paket | Projedeki görevi | İçe aktarıldığı dosya(lar) |
|---|---|---|
| `@petamoriken/float16` | `Float16Array` polyfill'i — TIFF örnek biçimi (SampleFormat) IEEE 754 half-float olan görüntü verilerini okumak için (`getFloat16`) | `src/dataview64.js`, `src/geotiffimage.js` |
| `lerc` | ESRI LERC (Limited Error Raster Compression) codec'inin kendisi — `compression: 34887` etiketli TIFF'leri çözmek için | `src/compression/lerc.js` |
| `pako` | zlib/DEFLATE decompression (`inflate`) — hem `Deflate`/`AdobeDeflate` sıkıştırması (8, 32946) hem de LERC'in iç kullanımı için | `src/compression/lerc.js`, `src/compression/deflate.js` |
| `quick-lru` | Sabit boyutlu LRU önbellek — `BlockedSource`'ta uzaktan/parçalı okumalarda getirilen blokları önbelleklemek için | `src/source/blockedsource.js` |
| `web-worker` | Tarayıcı `Worker` API'sinin Node.js'te de çalışan çapraz-platform polyfill'i — çözme (decode) işini worker thread'e taşımak için | `src/worker/create.js` |
| `xml-utils` | Hafif XML ayrıştırma yardımcıları (`findTagsByName`, `getAttribute`) — GDAL metadata / GeoTIFF gömülü XML etiketlerini ayrıştırmak için | `src/geotiffimage.js` |
| `zstddec` | Zstandard decompression — hem doğrudan zstd sıkıştırması (50000) hem de LERC'in iç zstd kullanımı için | `src/compression/lerc.js`, `src/compression/zstd.js` |
| `fs` *(Node.js çekirdek)* | Yerel dosya sisteminden okuma — sadece Node.js ortamında kullanılan `FileSource` | `src/source/file.js` |
| `http` / `https` / `url` *(Node.js çekirdek)* | Node.js'in yerleşik HTTP istemcisiyle uzak GeoTIFF getirme (`fetch`/`XHR` yerine) | `src/source/client/http.js` |

`package.json`'ın `browser` alanı bu dört Node.js çekirdek modülünü tarayıcı derlemesi için açıkça `false`'a eşliyor (`"browser": {"fs": false, "http": false, "https": false, "url": false}`) — bu, bunların kasıtlı olarak yalnızca-Node kod yolları olduğunu doğruluyor.

## 5. Sınırlamalar

- `@types/*` gibi yalnız-tip devDependency'ler bu depoda paket-seviyeli olarak `src/` içinden import edilmiyor (TS'in otomatik `@types` çözümlemesi ayrı bir mekanizma, AST'te görünür bir `import` üretmiyor); bu yüzden tabloda ayrı bir "yalnız-tip" satırı yok — bu beklenen bir durumdur, eksiklik değildir.
- Sonuçlar `package-lock.json`'daki sürümleri değil, yalnızca **paket kimliklerini** karşılaştırır (sürüm uyumluluğu kapsam dışı).
- Node.js çekirdek modül listesi, analiz çalıştırılan Node.js sürümünden (v22.23.1) alındı; ileride eklenecek yeni çekirdek modülleri farklı bir sürümde farklı sınıflandırılabilir.

## 6. Tekrarlanabilirlik

Kod: `analysis/tool/external-deps.mjs`. Ham veri: `analysis/data/external-deps.json`. Girdi commit ve araç sürümleri §A raporuyla aynıdır (bkz. `analysis/README.md`).
