# geotiff.rs — geotiff.js 3.1.0 Port ve Uyumluluk Sözleşmesi

Bu belge tarihsel bir yapılacaklar günlüğü değil, tamamlanan portun kapsamını ve kabul
ölçütlerini tanımlayan doğrulanabilir sözleşmedir.

- Referans uygulama: `../geotiff.js`
- Referans sürüm: `3.1.0`
- Referans commit: `8594d1b4bde4072326916185c848e73a9e704850`
- Rust release sürümü: `3.1.0` (referans sürümle sabit eşleşme)
- Hedef: native Rust/GPUI çalışma zamanı
- Son doğrulama tarihi: 2026-07-20
- Durum: native kapsam ve genişletilmiş iki-repo/corpus sertifikasyonu tamamlandı; yalnız gerçek
  browser WebP kapısı ortam bekliyor

Gerçek tarayıcıdaki `createImageBitmap` WebP karşılaştırması için in-app browser backend'i bu
oturumda kullanılamadı. Node oracle referansın browser-only hata yolunu, Rust testleri native
WebP sonucunu doğruluyor; gerçek browser kapısı çalıştırılana kadar bu tek madde “geçti” olarak
sayılmaz.

“Tam port”, JavaScript sınıflarını satır satır veya tarayıcı nesnelerini isim isim kopyalamak
anlamına gelmez. Aynı geçerli girdinin aynı metadata, raster, RGB, overview ve writer anlamını
üretmesi; aynı seçenek ve yeteneklerin native karşılıklarla erişilebilir olması; geçiş sırasında
verinin sessizce daralmaması veya atılmaması anlamına gelir. Rust'ın tip ve güvenlik modeliyle
zorunlu olan farklar bu belgede açıkça kayıtlıdır.

## 1. Kabul ölçütleri

Port ancak aşağıdaki koşullar birlikte sağlandığında eksiksiz sayılır:

1. geotiff.js 3.1.0'ın dışa açık kaynak, dataset, görüntü, metadata, codec, RGB, resample,
   iptal, önbellek ve writer yeteneklerinin native karşılığı bulunur.
2. Classic TIFF ve BigTIFF; iki byte sırası; tiled ve striped yerleşim; chunky ve planar
   configuration kayıpsız okunur.
3. Örnek seçimi, pencere, taşan pencere doldurma, interleaved/bant çıktısı, yeniden boyutlandırma
   ve overview seçimi gözlenebilir JS semantiğiyle uyuşur.
4. Kayıpsız codec çıktıları ve TIFF örnek değerleri JS oracle ile birebir uyuşur. Kayıplı JPEG
   için standartların izin verdiği bağımsız IDCT/upsampling yuvarlama farkı sınırlandırılır.
5. `u64/i64` değerler `f64` ara yoluna sokulmaz; JavaScript'in güvenli tamsayı sınırının üzerindeki
   BigTIFF değerleri Rust tarafında da tam kalır.
6. Geçersiz veya düşmanca girdiler panic, sonsuz döngü ya da kontrolsüz tahsis yerine normal hata
   döndürür. Bu güvenlik sertleştirmesi geçerli veri davranışını değiştirmez.
7. Rust testleri, canlı JS oracle testleri, Clippy ve dokümantasyon derlemesi temiz geçer.

## 2. Genel API eşlemesi

| geotiff.js 3.1.0 | geotiff.rs | Durum / not |
|---|---|---|
| `fromUrl` | `from_url*` | Header, multi-range, full-file ve block seçenekleri dahil |
| `fromCustomClient` | `from_custom_client*`, `from_reader*` | `Arc<dyn AsyncFileReader>` sözleşmesi |
| `fromArrayBuffer` | `from_array_buffer*`, `from_bytes*` | `bytes::Bytes` veya dönüştürülebilir sahipli veri |
| `fromFile` | `from_file*` | Asenkron yerel range reader |
| `fromBlob` | `from_blob*` | Native ortamda blob içeriğinin kayıpsız byte karşılığı |
| `fromUrls` | `from_urls*` | Ana dosya + sırası korunan harici overview dosyaları |
| `GeoTIFF` | `SingleGeoTiff` | Bir dosyanın tüm IFD'leri |
| `MultiGeoTIFF` | `MultiGeoTiff` | Dosyalar arasında tek global görüntü index'i |
| `GeoTIFFBase` | `GeoTiffDataset` | `Single`/`Multi` kapalı enum'u |
| `GeoTIFFImage` | `GeoTiffImage` | Görüntü metadata/raster/RGB API'sinin tamamı |
| `ImageFileDirectory` | `FileDirectory` | İsim veya sayıyla tag erişimi, kayıpsız değerler |
| `Pool` / worker | özel Rayon decode havuzu / doğrudan `get_decoder` | Pozitif boyutta worker eşlemesi; `Pool(0)` için aynı inline decode yolu |
| `AbortSignal` | `CancellationToken` | I/O sırasında ve decode öncesi/sonrası iptal |
| `BaseClient` / `BaseResponse` | `AsyncFileReader`, `HttpRangeReader` | Native range-I/O sözleşmesi |
| `addDecoder` / `getDecoder` | `add_decoder` / `get_decoder` | Built-in override, özel ID ve JS ile aynı lookup hataları; `find_decoder` hatasız probe ekidir |
| `registerTag` | `register_tag` | Parser tarafından da kullanılan çalışma zamanı kaydı |
| `setLogger` | `set_logger` | Değiştirilebilir global logger |
| `writeArrayBuffer` | `write_array_buffer` | JS wire düzeniyle uyumlu çıktı |
| `writeGeotiff` | `write_geotiff` | Düz typed veya `[band][row][column]` veri |
| `globals`, `rgb`, `resample`, `predictor`, `utils` | aynı adlı Rust modülleri | Saf yardımcıların native karşılıkları |

Rust'taki `*_with_options`, `from_object`, `PackedSampleMode::Lossless`, 64 bit tam raster ve
JPEG2000 yolları referans API'yi daraltmaz; native kullanım için ek yeteneklerdir.

## 3. Kaynak ve range-I/O uyumluluğu

### 3.1. Desteklenen kaynaklar

- Bellek (`ArrayBuffer`/`Blob` byte karşılığı)
- Yerel dosya
- HTTP/HTTPS COG URL'si
- Özel `AsyncFileReader`
- S3/GCS/Azure/yerel object store (`object_store`)
- Ana URL ile bir veya daha çok dış overview URL'si

### 3.2. HTTP seçenekleri

`HttpSourceOptions`, JS `SourceOptions` alanlarını karşılar:

- `headers`
- `max_ranges`
- `allow_full_file`
- `block_size`
- `cache_size`

Reader şunları ayrıca doğrular:

- `206 Content-Range` başlangıcı, sonu, toplam uzunluğu ve body uzunluğu
- EOF'ta yasal kısa son range
- EOF olmayan kısa `206` cevabının reddedilmesi
- `416` ve `bytes */size` davranışı
- multipart boundary ve istek sırasının korunması
- sunucunun yalnızca ilk range'i döndürmesi durumunda kalan range'lerin ayrı alınması
- `Range` değerlerinin transfer sıkıştırmasından etkilenmemesi için `Accept-Encoding: identity`

`BlockedReader`, JS `BlockedSource` davranışını aligned block okuma, LRU saklama ve eşzamanlı aynı
isteği tekilleştirme ile sağlar. `GeoTiffOptions` içindeki ikinci cache katmanı sıkıştırılmış
tile/strip range'lerini byte ağırlıklı kapasiteyle tutar. `{ cache: true }` ise decode edilmiş
blokları görüntü bazında saklar ve eşzamanlı decode'ları birleştirir.

### 3.3. Kaynak ömrü

JS `close()` yerine Rust RAII kullanır. `close(self)` açık bir uyumluluk metodu olarak da vardır;
dataset tüketildiğinde dosya, ağ ve object-store handle'ları düşürülür.

## 4. TIFF, IFD ve GeoTIFF metadata kapsamı

### 4.1. Container ve alan tipleri

- Classic TIFF (`42`) ve BigTIFF (`43`)
- Little-endian (`II`) ve big-endian (`MM`)
- Bağlı birden fazla IFD ve geçerli next-IFD zinciri
- `BYTE`, `ASCII`, `SHORT`, `LONG`, `RATIONAL`, signed karşılıkları, `FLOAT`, `DOUBLE`,
  `UNDEFINED`, `IFD`, `LONG8`, `SLONG8`, `IFD8`
- Inline ve offset'te tutulan değerler
- Bilinen, vendor/private ve çalışma zamanında kaydedilen tag'ler
- Scalar/array ayrımının tag tanımına göre korunması

`FileDirectory` şu JS yöntemlerinin karşılığını sunar: `hasTag`, `getValue`, `loadValue`,
`loadValueIndexed`, `parseGeoKeyDirectory`, `toObject`. Rust parser tüm değerleri açılışta
actualize ettiği için `load_value` ek I/O gerektirmez. `load_value_indexed`, JS'nin RATIONAL ve
SRATIONAL typed-array görünümünü de korur: pay ve payda ayrı düz indekslerdir.

### 4.2. GeoKey ve yardımcı metadata

- Bütün bilinen GeoKey adları ve ID'leri
- Bilinmeyen/private GeoKey'lerin sayısal ID ile korunması
- Inline SHORT, `GeoDoubleParams`, `GeoAsciiParams` ve diğer geçerli tag referansları
- ASCII ayırıcı/NUL semantiği
- `ModelPixelScale`, `ModelTiepoint`, `ModelTransformation`
- `GDAL_METADATA`, sample filtresi ve JavaScript `Number(...)` attribute semantiği
- `GDAL_NODATA`
- COG `GDAL_STRUCTURAL_METADATA_SIZE` ghost alanı
- `origin`, `resolution`, `bounding_box`, `pixel_is_area`

64 bit offset, count, tag ve raster değerleri `u64/i64` olarak korunur. JS'in
`Number.MAX_SAFE_INTEGER` üzerindeki hata/daralma sınırı native portta veri kaybına dönüştürülmez.

## 5. Raster okuma sözleşmesi

### 5.1. Yerleşimler

- Tiled ve striped TIFF
- Chunky (`PlanarConfiguration=1`) ve planar (`=2`)
- Kenar tile'larının gerçek genişlik/yüksekliği
- Eksik `RowsPerStrip` için JS fallback'i
- Sıfır byte-count bloklarda nodata/0 doldurma
- Yalnız pencereyle kesişen blokların fetch edilmesi

### 5.2. Örnek biçimleri

- Unsigned ve signed 1–64 bit tamsayı örnekler
- Packed/non-byte-aligned örnekler; satır sonu padding'i
- Float16, Float32 ve Float64
- Örnek başına farklı `BitsPerSample`/`SampleFormat`
- Predictor 1 (none), 2 (horizontal) ve 3 (floating point)
- Byte sırası ve predictor tersine çevirme sırasının tiled/striped yollarda aynı olması

Bloktan çıktı dizisine kopyalama ve nearest resample, tamsayıları `f64` üzerinden geçirmez.
Böylece `9_007_199_254_740_993u64` gibi değerler bant ve interleaved sonuçlarda tam kalır.

### 5.3. `readRasters`

`ReadRastersOptions` şu alanların tamamını taşır:

- `window`
- `samples`
- `interleave`
- `width`, `height`
- `resample_method` (`nearest`, `bilinear`, büyük/küçük harf duyarsız)
- `fill_value` (scalar veya bant başına)
- çağrı bazlı `decoder_registry`
- `cancellation`
- `packed_sample_mode`

Varsayılan çıktı JS gibi ayrı bant dizileridir. `interleave=true` tek typed array üretir.
Negatif veya görüntü dışına taşan geçerli pencere kesişen kısmı okur ve kalan alanı JS
truthiness/doldurma kurallarıyla doldurur. Ters veya taşan boyutlar normal hata döndürür.

### 5.4. Packed örnek politikası

geotiff.js 3.1.0'ın multi-sample packed chunky kopyalama yolunda fractional byte-offset kullanan
tarihsel bir hata vardır. Örneğin üç aynı 12 bit düzlemden ikisini yanlış okuyabilir. İki ihtiyaç
birlikte sağlanır:

- `PackedSampleMode::Lossless` (varsayılan): kaynaktaki bütün örnekleri doğru korur.
- `PackedSampleMode::GeotiffJs`: eski JS 3.1.0 çıktısını byte düzeyinde tekrarlar.

Bu seçim güvenli migration için gereklidir: varsayılan veri kaybettirmez, eski çıktıya bağımlı bir
uygulama ise açıkça compatibility moduna geçebilir.

### 5.5. Yeniden örnekleme

- Bant ve interleaved nearest
- Bant ve interleaved bilinear
- JS koordinat/yuvarlama formülleri
- Sample grubunun interleaved çıktıda birlikte kalması
- Yalnız bir eksen verilirse diğer eksenin native/pencere boyutunu koruması

Son madde geotiff.js 3.1.0'daki boş typed-array üreten tek-eksen resize hatasını güvenli şekilde
düzeltir; dokümante edilmiş seçeneğin amaçlanan yeteneğini korur, veri kaybını taklit etmez.

## 6. Codec uyumluluk matrisi

| TIFF Compression | ID | Rust yolu | Uyumluluk |
|---|---:|---|---|
| None / Raw | 1 | native raw decoder | Birebir |
| LZW | 5 | port edilmiş güvenli LZW | Birebir; bozuk stream panic değil hata |
| Old JPEG | 6 | desteklenmez | geotiff.js de açıkça desteklemez |
| JPEG | 7 | `zune-jpeg`, TIFF component space | YCbCr subsampling dahil; kayıplı yuvarlama toleransı |
| Deflate | 8 | native Deflate | Birebir |
| Adobe Deflate | 32946 | aynı Deflate decoder | Birebir |
| PackBits | 32773 | port edilmiş PackBits | Birebir |
| LERC | 34887 | native LERC | Raw, LERC+Deflate, LERC+Zstd birebir |
| Zstandard | 50000 | native Zstd | Birebir |
| WebP | 50001 | native WebP | RGB ve RGBA; browser Canvas bağımlılığı yok |
| JPEG2000 | 34712 | native OpenJPEG | Rust'a özgü ek yetenek |

Kullanıcı özel/private compression ID'leri `DecoderRegistry` ile kaydedebilir; built-in decoder
aynı yöntemle override edilebilir. Doğrudan `get_tile_or_strip_with_registry`, `read_rasters`,
`read_rgb` ve best-fit okuma çağrı bazlı registry kabul eder.

JPEG geçerli şekilde decode edilir ve JS'e eşdeğer component space döndürür. Bununla birlikte
geotiff.js'in saf JS decoder'ı ile zune-jpeg'in IDCT ve chroma interpolation yuvarlamaları birkaç
kayıplı pikselde byte düzeyinde farklı olabilir. Testler maksimum ve ortalama sapmayı bağımsız
kayıpsız referansa karşı sınırlar; bant, renk uzayı, boyut veya özellik kaybı yoktur.

## 7. RGB dönüşümü

`readRGB`/`ReadRgbOptions` şu seçenekleri destekler: window, interleave, width, height,
resample method, `enableAlpha`, çağrı bazlı decoder registry, packed policy ve cancellation.

| PhotometricInterpretation | Davranış |
|---|---|
| WhiteIsZero (0) | Gri → RGB, ters ölçek |
| BlackIsZero (1) | Gri → RGB |
| RGB (2) | RGB doğrudan; istenirse gerçek alpha sample'ı |
| Palette (3) | ColorMap → RGB |
| CMYK (5) | CMYK → RGB |
| YCbCr (6) | YCbCr → RGB |
| CIELab (8) | CIELab → RGB |

`Uint8` dönüşümleri ECMAScript ToUint8 ve ToUint8Clamp kurallarını, yarım değerlerde ties-to-even
dahil, korur. Geçersiz palette index'i JS'teki sıfır davranışını üretir ve panic yapmaz.

## 8. Dataset, overview ve coğrafi seçim

- `getImage`/`image` ve `getImageCount`/`image_count`
- Ana dosyadaki dahili IFD/overview'lar
- `fromUrls` ile dış overview dosyaları
- Dosya sınırları üzerinde global image index'i
- `NewSubfileType` ve legacy `SubfileType` overview tanıma
- `resX`, `resY`, `bbox`, `width`, `height` ile JS best-fit algoritması
- Bbox → pixel-window dönüşümünde JS yuvarlama kuralı
- `window`/`bbox` ve `width`/`resX` gibi çelişkili seçeneklerin JS mesajıyla reddi
- Seçilen overview'a sample, interleave, fill, resample, decoder ve cancellation seçeneklerinin
  eksiksiz aktarılması

## 9. Writer sözleşmesi

Writer geotiff.js 3.1.0'ın wire seçimlerini korur:

- Classic, big-endian TIFF
- Tek IFD ve 1000 byte IFD rezervi
- Düz typed array, düz JS-number karşılığı veya `[band][row][column]` giriş
- Unsigned, signed, Float32 ve Float64 sample verisi
- Striped, multi-strip, tiled, chunky ve planar sıra
- Tag varsayımları ve metadata override'ları
- `GeoKeyDirectory`, `GeoDoubleParams`, `GeoAsciiParams`
- `GDAL_NODATA`, model tiepoint/scale/transformation
- JS writer'ın NUL/count tel biçimi

IFD rezervini aşan metadata sessizce pixel verisinin üzerine yazmak yerine hata döndürür. Düz
number dizisi, bütün typed-array türleri, multi-strip, tiled chunky/planar, tiled Float64 ve
sıfır byte-count/nodata örneklerinde Rust writer çıktısı canlı JS çıktısıyla wire-byte düzeyinde
karşılaştırılır; her iki çıktı kendi reader'ında tekrar açılarak raster hash'i de eşitlenir.

## 10. Native çalışma zamanı eşlemeleri

| JS mekanizması | Native karşılık | Davranış gerekçesi |
|---|---|---|
| Promise tabanlı source | Tokio + `AsyncFileReader` | Aynı asenkron range-I/O |
| Web Worker `Pool` | özel Rayon pool | CPU decode UI/I/O thread'ini bloklamaz |
| Pool job kuyruğu | Tokio semaphore | Sınırsız sıkıştırılmış iş yığılması engellenir |
| `AbortSignal` | `CancellationToken` | Bekleyen I/O düşürülür; decode öncesi/sonrası kontrol |
| Browser `Blob` | `Bytes` | Native ortamda aynı sahipli byte içeriği |
| `createImageBitmap` WebP | native libwebp | Aynı raster yeteneği, browser gerektirmez |
| JS sınıf kalıtımı | Rust struct + enum | Aynı iki dataset çeşidi, statik tip güvenliği |
| `close()` | RAII + tüketen `close(self)` | Kaynak ömrü deterministik |

Decode başlamış bir native codec zorla preempt edilmez. İptal edilen iş decode kuyruğuna girmez;
çalışırken iptal edilirse sonuç atılır ve sonraki bloklar fetch edilmez. Havuz thread sayısı ilk
kullanımdan önce `configure_decode_pool` ile ayarlanabilir; varsayılan sistem parallelism'idir.
JS'deki `new Pool(0).bindParameters(...).decode(...)` worker oluşturmayan yolun native karşılığı,
`get_decoder` ile alınan decoder'ı doğrudan çağırmaktır; `configure_decode_pool(0)` ise Rayon'ın
otomatik thread sayısını seçer. `destroy()` karşılığı doğrudan decoder'ın düşürülmesi/RAII'dır.

## 11. Bilinçli ve güvenli farklar

Bu farklar özellik veya veri kaybı değildir:

1. **Eager metadata:** JS deferred IFD değerleri gerektiğinde yükler; Rust tüm IFD zincirini
   açılışta doğrular. Aynı değerler sunulur, daha sonraki erişim I/O'suzdur.
2. **Tam 64 bit:** JS `Number` güvenli sınırın üzerinde hata verebilir veya kesinlik kaybedebilir;
   Rust değerleri tam tutar.
3. **Hata güvenliği:** Bozuk offset/count, allocation overflow, kısa codec stream, geçersiz pencere
   ve malformed metadata panic/undefined davranışı yerine `Result::Err` üretir.
4. **Packed JS hatası:** Kayıpsız davranış varsayılandır; birebir eski çıktı açık compatibility
   modunda vardır.
5. **Tek-eksen resize JS hatası:** Rust eksik ekseni koruyup gerçek data üretir; JS 3.1.0'ın boş
   çıktı hatası taklit edilmez.
6. **Malformed tiepoint:** Altının katı olmayan dizi Rust'ta hata olur; JS trailing `undefined`
   alanlı nesne üretir.
7. **Kayıplı JPEG:** Bağımsız decoder'ın izinli yuvarlama farkları olabilir; tolerans kapısı ve
   kayıpsız referans karşılaştırması vardır.
8. **Ek native kaynak/codec:** Object store ve JPEG2000 JS yüzeyini genişletir, değiştirmez.
9. **IFD index referans hatası:** JS, 1'den büyük ve var olmayan bir IFD istendiğinde recursive arama
   nedeniyle istenen index yerine ilk eksik index olan `1`'i raporlayabilir. Rust'ın typed hatası
   gerçekten istenen index'i korur.
10. **Geçersiz blok sample/koordinatı:** JS chunky blokta aralık dışı sample etiketini kabul eder;
    Rust tutarsız sonucu reddeder. Tile koordinatı bağımlılığa girmeden doğrulanır; böylece aynı
    geçersiz çağrı normal hata olur, panic olmaz.
11. **Dinamik geçersiz nesneler:** Boş writer girdisinin incidental JS `TypeError`'ı stabil
    `WriterError::EmptyData` olur. Abstract source ve yanlış türde GeoKey/tag metadata'sı Rust
    trait/enum sözleşmesiyle çalışma zamanına ulaşmadan engellenir.
12. **BigTIFF reserved alanı:** JS sıfır olmayan reserved word'ü kontrol etmez; Rust standardın
    zorunlu sıfır değerini doğrular.

Koordinat sistemi reprojeksiyonu, harita renderer'ı ve GPU texture yönetimi geotiff.js 3.1.0'ın da
görevi değildir; bu crate'in port kapsamına dahil değildir.

## 12. Doğrulama kanıtı

### 12.1. Referans JS build'i

2026-07-20'de referans commit üzerinde şu temiz akış çalıştırıldı:

```text
Node.js v26.5.0
npm 11.17.0
npm ci --ignore-scripts
npm run build
npm ls --depth=0
```

Sonuç: exit code 0; `dist-module`, `dist-node` ve `dist-browser` başarıyla üretildi. Görülen
Browserslist/caniuse-lite mesajı yalnız bakım uyarısıdır, build hatası değildir. Tam `npm audit`
eski dev/build zincirinde 37 bulgu raporladı; `npm audit --omit=dev` üretim bağımlılıklarında
`0 vulnerabilities` verdi. Bunlar port doğruluğundan ayrı bağımlılık bakım konularıdır.

`bildirim.md` içindeki hata geotiff.js build hatası değildir. Kurulu `async-tiff 0.3.0`
`TokioReader::make_range_request` kodunda `Vec::with_capacity` ile uzunluğu sıfır kalan buffer'a
`read(&mut [u8])` yapılmasıdır. `LocalFileReader`, boyutlandırılmış buffer + `read_exact` ile bu
upstream hatasını port içinde tamamen dolaşır.

### 12.2. Rust test kapıları

Son tam kapıda:

- 188 birim testi
- 5 varsayılan entegrasyon testi (4 fixture paritesi + tile-coordinate panic regresyonu)
- 5 ayrı canlı iki-repo differential matrisi (dördü sentetik/yerel fixture, biri upstream corpus)
- Varsayılan kapıda toplam 193 test; ardından 5 canlı oracle testi, 0 başarısız
- `cargo clippy --offline --all-targets -- -D warnings`: temiz
- `cargo doc --offline --no-deps`: temiz
- `cargo fmt --check` ve `git diff --check`: temiz

Parite fixture'ları şu alanları kapsar:

- Planar RGB, tiled 1-bit gray, striped gray
- 1/4/12 bit packed palette ve RGB
- Int8, Int16, Float16, Float32
- Predictor 2 ve 3
- LZW, Deflate, PackBits, Zstd
- LERC raw/Deflate/Zstd
- WebP RGB/RGBA
- JPEG RGB ve 4:2:0 YCbCr
- Alpha sample
- Pencere, sample seçimi, interleave, fill, nearest/bilinear ve RGB
- Classic TIFF/BigTIFF, tam `u64` raster
- Writer wire-byte oracle ve round-trip
- WhiteIsZero, CMYK, CIELab ve RGBA alpha dispatcher yolları
- HTTP range/multipart/full-file/EOF güvenliği
- Multi-file overview ve best-fit seçimi
- Decoder override, decoded cache, `Pool(0)` inline decode, logger ve cancellation
- Header, image/sample/tile, option, codec ve writer hata sözleşmeleri

Kayıpsız fixture hash'leri canlı geotiff.js 3.1.0 çalıştırmasından alınmıştır. JPEG testi hem
JS'nin hem Rust'ın kayıpsız kaynak görüntüye sapmasını ölçer. Writer testleri canlı JS tarafından
üretilen byte düzenini sabit oracle olarak kullanır.

### 12.3. GeoTIFF/test-data corpus kapısı

Upstream `GeoTIFF/test-data` deposu
`8506204783ff26a6c49ed1f721e7e1635b2e43ee` commit'ine sabitlendi; 21 doğrudan TIFF ve ZIP'ten
çıkarılan bir TIFF aynı anda canlı geotiff.js 3.1.0 ile Rust portuna okutuldu. Toplam 22 dosya ve
32 görüntünün tamamı eşleşti. Dosya/IFD/tag metadata'sı, tam veya açıkça sınıflandırılmış büyük
görüntü örnekleri, bant/interleaved pencereler, sample seçimi, fill, nearest/bilinear, doğrudan
bloklar, RGB/alpha ve dataset best-fit yolları karşılaştırılır. Ayrıntılı kapsam, kaynak politikası
ve provenance incelemesi `tests/differential/TEST_DATA_REPORT.md` içindedir.

Corpus iki BigTIFF, iki endian, tiled/striped görüntüler, 8/16/32 bit unsigned/signed/float,
uncompressed/LZW/Deflate/PackBits, Predictor 1/3 ve gray/RGB/palette verilerini kapsar. Bu kapı
GDAL NoData `-inf` değerinde Rust parser'ının JS `Number()` davranışından ayrıldığını buldu;
`parse_js_number` düzeltildi ve regresyon testi eklendi.

### 12.4. Tekrarlanabilir son kapı

```bash
cargo fmt --all -- --check
cargo check --offline --all-targets
cargo test --offline --all-targets
cargo clippy --offline --all-targets -- -D warnings
cargo doc --offline --no-deps
GEOTIFF_TEST_DATA_DIR=/tmp/geotiff-test-data \
  cargo test --offline --test test_data_differential -- --ignored --nocapture
git diff --check
git status --short
git -C ../geotiff.js status --short
```

Disk kullanımını sınırlamak için her büyük doğrulama/commit diliminden sonra `cargo clean`
çalıştırılır.

## 13. Mimari ve bağımlılıklar

```text
Memory / File / HTTP / ObjectStore / custom AsyncFileReader
                         |
             lossless metadata discovery
                         |
       compressed byte-range + decoded-block caches
                         |
             tiled / striped block planner
                         |
             dedicated Rayon decode pool
                         |
       codec -> predictor -> typed sample planes
                         |
       window/fill -> resample -> raster or RGB
                         |
          Single / Multi dataset API
```

Başlıca karşılıklar:

| JS bağımlılığı / mekanizması | Rust karşılığı |
|---|---|
| `@petamoriken/float16` | `half` |
| `pako` | native Deflate |
| `lerc` | `lerc` + async-tiff adapter'ı |
| `zstddec` | `zstd` |
| `quick-lru` | `moka` |
| `web-worker` | `rayon` + Tokio semaphore |
| `xml-utils` | `roxmltree` |
| Fetch/XHR/Node HTTP | `reqwest` range reader |
| Node fs / FileReader | native async file reader |
| storage adapter'ları | `object_store` |

`async-tiff 0.3.0` IFD/tag modeli ve decoder trait'i için kullanılır; metadata kesinliği,
striped/packed örnekler, predictor, güvenli codec adaptörleri ve kaynak sözleşmesi bu crate içinde
tamamlanır. `object_store` sürümü async-tiff'in trait sürümüyle uyumlu kalması için `0.13` hattında
tutulur.

## 14. Tamamlanma kontrol listesi

- [x] Referans sürüm/commit sabitlendi
- [x] Public export ve yöntem yüzeyi eşlendi
- [x] Classic TIFF/BigTIFF ve iki endian
- [x] Bütün TIFF field type'ları ve kayıpsız tag modeli
- [x] GeoKey/GDAL/COG ghost metadata
- [x] Tiled/striped, chunky/planar
- [x] Packed ve 1–64 bit örnekler, Float16/32/64
- [x] Predictor 1/2/3
- [x] JS codec seti ve özel decoder kaydı
- [x] Raster window/sample/interleave/fill/resample
- [x] RGB ve alpha
- [x] Dahili/harici overview ve best-fit
- [x] Cache, decode pool ve cancellation
- [x] Writer wire uyumluluğu ve round-trip
- [x] Canlı JS oracle testleri
- [x] Geçerli çıktıların iki repoda aynı operasyonla karşılaştırılması
- [x] Hata ve düşmanca public girdi differential matrisi
- [x] Sabitlenmiş GeoTIFF/test-data corpus differential matrisi (22 dosya / 32 görüntü)
- [ ] Gerçek browser `createImageBitmap` WebP differential kapısı (backend bekleniyor)
- [x] JS 3.1.0 temiz build doğrulaması
- [x] Rust test/Clippy/doc kalite kapıları
- [x] Düzenli açıklamalı commit ve `cargo clean`

## 15. Kaynak ve bakım notları

- İlk bağımlılık grafiği ve tarihsel topolojik sıra `analysis/` altında korunur.
- Fixture provenance ve lisans bilgisi `tests/fixtures/README.md` içindedir.
- `async-tiff` TokioReader upstream bildirim taslağı `bildirim.md` içindedir.
- Referans JS commit'i değiştirildiğinde bu sözleşme otomatik olarak yeni sürüme uygulanmış
  sayılmaz. Export/method diff'i, canlı oracle hash'leri, build ve bütün Rust kapıları yeniden
  çalıştırılmalıdır.
- Codec veya parser bağımlılığı yükseltilirse özellikle JPEG component space, LERC parametreleri,
  BigTIFF 64 bit değerleri, packed samples ve object_store trait sürümü tekrar doğrulanmalıdır.
