# A. İç Bağımlılık Analizi — geotiff.js

**Kapsam:** `geotiff.js` reposunun `src/` dizini (36 dosya: 34 `.js` + 2 `.ts`; `test/` dahil değil).
**Commit:** `8594d1b4bde4072326916185c848e73a9e704850` (2026-05-26)
**Araç:** ts-morph 28.0.0 (TypeScript 7.0.2 derleyici API'si üzerinde), Node.js v22.23.1, `node:sqlite` (deneysel).
**Analiz tarihi:** 2026-07-19. Tam metodoloji ve tekrar-çalıştırma adımları için bkz. `analysis/README.md`.

Not: `src/` içindeki `.js` dosyaları da `tsconfig.json`'da `checkJs: true` ile derleniyor; bu yüzden derleyici JSDoc tip açıklamalarını (`@param {BaseSource} x` gibi) da anlamlı/çözümlenebilir düğümler olarak işliyor. Bu analizde find-references bu nedenle sadece `.ts` dosyalarında değil tüm `.js` dosyalarında da tam sembol çözümlemesiyle çalıştı (metin araması değil).

---

## 1. Yöntem uygulaması (özet)

| Adım | Ne yapıldı |
|---|---|
| 1. Eleman envanteri | `src/**/*.{js,ts}` AST'sinde gezildi; sınıflar, kurucular, metotlar/erişimciler (get/set), serbest fonksiyonlar (iç içe olanlar dahil) ve fonksiyon-değerli değişkenler (`const f = (...) => ...`) katalogland. Her elemana `file+konum` tabanlı kimlik verildi. Fonksiyon-değerli değişkenlerde hem `VariableDeclaration` hem de ok-fonksiyonu/`FunctionExpression` düğümü aynı kimliğe eşlendi. |
| 2. Referans toplama + düzeltme | Her elemanın ad düğümünde derleyicinin `findReferences()`'ı çalıştırıldı. Her referans girdisi için `checker`'dan (`getSymbolAtLocation` eşdeğeri) sembol çözümlendi, alias/import zinciri sonuna kadar takip edildi (`getAliasedSymbol`, gerekirse çok adımlı), ve sembolün **gerçek bildirim düğümü** kataloğumuzdaki elemanla eşleşiyorsa sayıldı. Eşleşmiyorsa (kalıtım hiyerarşisindeki başka bir bildirime aitse) atlandı — bu, aşağıdaki §3'te somut örnekle doğrulanan düzeltmedir. |
| 3. Çağıran ataması | Her referansın konumunu içeren **en küçük kayıtlı eleman aralığı** çağıran olarak atandı (iç içe fonksiyonlarda en içteki kazanır). Hiçbir elemanın içinde olmayan referanslar (import satırları, modül üst düzeyi) çağıransız bırakıldı. Kendine referanslar (`caller_id === callee_id`) atıldı; aynı (çağıran→çağrılan) çifti tek kenara indirgendi (çoklu çağrı satırı `occurrences` alanında sayıldı ama kenar sayısını şişirmedi). |
| 4. Depolama | SQLite (`analysis/data/analysis.sqlite`, `node:sqlite` ile), `elements` ve `edges` tabloları. |
| 5. Doğrulama | Aşağıda §3. |

**Ek işlenen özel durumlar (spesifikasyonun kapsamındaki doğal uzantılar):**
- `new ClassName(...)` çağrıları, sınıfın açık bir kurucusu varsa çağrı **kurucu elemanına** yönlendirildi (checker `new X()`'i sınıf sembolüne çözümlediği için bu yönlendirme elle yapıldı — 37 referans bu şekilde yeniden atandı).
- `super(...)` çağrıları ayrı bir AST taramasıyla ele alındı (`super` bir anahtar kelime olduğu, ayrı bir sembole sahip olmadığı için `findReferences` ile yakalanamaz); üst sınıfın açık kurucusu varsa oraya kenar eklendi (4 kenar).

---

## 2. Envanter özeti

| Tür | Sayı |
|---|---|
| Sınıf (`class`) | 41 |
| Kurucu (`constructor`) | 30 |
| Metot/erişimci (`method`) | 142 |
| Serbest fonksiyon (`function`, iç içe dahil) | 110 |
| Fonksiyon-değerli değişken (`function-var`) | 6 |
| **Toplam eleman** | **329** |
| **Toplam kenar (tekil çağıran→çağrılan çifti)** | **421** |

**Ölçü birimi tanımı:** Tüm "bağımlı sayısı" değerleri **tekil çağıran eleman sayısıdır**, çağrı satırı sayısı değil. Örn. bir metot aynı çağrılan işlevi kendi gövdesinde 3 kez çağırırsa bu 1 kenar olarak sayılır (`occurrences=3` meta verisiyle birlikte tutulur, ama "bağımlı sayısı" 1 artırır).

### İşlem istatistikleri (şeffaflık için)

| Metrik | Değer | Açıklama |
|---|---|---|
| Ham referans girdisi (tanım hariç) | 984 | Tüm elemanların `findReferences()` sonuçlarının toplamı |
| → Kalıtım-birleşmesi nedeniyle başka elemana ait olduğu için atlandı | 264 | §3'te doğrulanan düzeltme |
| → Katalog dışına çözümlendi (dış API, parametre, vb.) | 3 | Bkz. §4, destructuring örneği |
| → Sembol hiç çözümlenemedi (işlenen elemana sayıldı, bilinen hata payı) | 0 | Bu çalıştırmada oluşmadı |
| → Çağıransız kaldı (import satırı / modül üst düzeyi) | 160 | Bkz. §4 |
| → Kendine referans, atıldı | 4 | `GeoTIFF.requestIFD`, `GeoTIFFImage`, `GeoTIFFImage.getResolution`, `toArrayRecursively` (hepsi gerçek özyineleme) |
| → Kurucu yönlendirmesi (`new X()` → kurucu) | 37 | |
| → `super(...)` özel taraması | 4 | |
| **Sonuç: tekil kenar** | **421** | |

---

## 3. Sorgu 1 — En çok bağımlı olunan 25 eleman

| Bağımlı sayısı | Eleman | Tür | Konum |
|---:|---|---|---|
| 19 | `ImageFileDirectory.getValue` | method | src/imagefiledirectory.js:314 |
| 9 | `BaseDecoder` | class | src/compression/basedecoder.js:13 |
| 9 | `BaseSource` | class | src/source/basesource.js:9 |
| 9 | `GeoTIFFImage.getHeight` | method | src/geotiffimage.js:247 |
| 8 | `GeoTIFFImage.getWidth` | method | src/geotiffimage.js:239 |
| 7 | `GeoTIFF` | class | src/geotiff.js:397 |
| 7 | `ImageFileDirectory.hasTag` | method | src/imagefiledirectory.js:300 |
| 6 | `BaseSource.fetch` | method | src/source/basesource.js:15 |
| 6 | `GeoTIFF.fromSource` | method | src/geotiff.js:563 |
| 6 | `ImageFileDirectory.loadValue` | method | src/imagefiledirectory.js:339 |
| 5 | `DataSlice.readUint32` | method | src/dataslice.js:84 |
| 5 | `resolveTag` | function | src/globals.js:288 |
| 4 | `AbortError.constructor` | constructor | src/utils.ts:161 |
| 4 | `BaseClient` | class | src/source/client/base.js:34 |
| 4 | `BaseResponse` | class | src/source/client/base.js:1 |
| 4 | `DataSlice.constructor` | constructor | src/dataslice.js:8 |
| 4 | `DataSlice.readUint64` | method | src/dataslice.js:124 |
| 4 | `ImageFileDirectory` | class | src/imagefiledirectory.js:278 |
| 4 | `RemoteSource.constructor` | constructor | src/source/remote.js:28 |
| 4 | `copyNewSize` | function | src/resample.js:11 |
| 4 | `decodeScan>decodeHuffman` | function (nested) | src/compression/jpeg.js:180 |
| 4 | `decodeScan>readBit` | function (nested) | src/compression/jpeg.js:163 |
| 4 | `decodeScan>receiveAndExtend` | function (nested) | src/compression/jpeg.js:213 |
| 4 | `getFieldTypeSize` | function | src/globals.js:56 |
| 4 | `maybeWrapInBlockedSource` | function | src/source/remote.js:181 |

**Yorum:** Sonuçlar sezgiyle örtüşüyor — `ImageFileDirectory.getValue` TIFF etiket okumanın merkezi düşük seviye erişimcisi; `BaseDecoder`/`BaseSource`/`BaseClient`/`BaseResponse` sırasıyla sıkıştırma, kaynak ve HTTP istemci hiyerarşilerinin temel sınıfları; `GeoTIFFImage.getWidth/getHeight` görüntü boyutlarının her yerde kullanılan erişimcileri.

`BaseSource`'un 9 bağımlısının kaynağı: 5 gerçek `extends BaseSource` ilişkisi (`FileSource`, `RemoteSource`, `ArrayBufferSource`, `FileReaderSource`, `BlockedSource`) + `checkJs` altında tip-çözümlenen 4 JSDoc `@param {BaseSource} ...` / `@returns {BaseSource}` açıklaması (geotiff.js, geotiffimage.js, imagefiledirectory.js, remote.js içinde). Bu, aracın metin araması değil gerçek tip çözümlemesi yaptığının bir kanıtıdır (yorum satırları find-references'a hiç girmez, ama JSDoc tip açıklamaları `checkJs` altında derleyici için gerçek tip konumlarıdır).

## 4. Sorgu 2 — "Yaprak ama temel" elemanlar (giden bağımlılığı yok, çok bağımlı olunuyor)

> **Bağımsızlık tanımı burada yalnızca izlenen 329 elemana göredir** — bu elemanların dış kütüphane çağrıları (`Math.*`, `Array.prototype.*`, DOM/Node API'leri vb.) olabilir, onlar kapsam dışıdır.

| Bağımlı sayısı | Eleman | Tür | Konum |
|---:|---|---|---|
| 9 | `BaseDecoder` | class | src/compression/basedecoder.js:13 |
| 9 | `BaseSource` | class | src/source/basesource.js:9 |
| 5 | `DataSlice.readUint32` | method | src/dataslice.js:84 |
| 5 | `resolveTag` | function | src/globals.js:288 |
| 4 | `BaseResponse` | class | src/source/client/base.js:1 |
| 4 | `DataSlice.constructor` | constructor | src/dataslice.js:8 |
| 4 | `RemoteSource.constructor` | constructor | src/source/remote.js:28 |
| 4 | `copyNewSize` | function | src/resample.js:11 |
| 4 | `decodeScan>readBit` | function (nested) | src/compression/jpeg.js:163 |
| 4 | `getFieldTypeSize` | function | src/globals.js:56 |
| 3 | `BaseResponse.get:status` | method | src/source/client/base.js:13 |
| 3 | `DataSlice.readUint16` | method | src/dataslice.js:64 |
| 3 | `arrayForType` | function | src/geotiffimage.js:38 |
| 3 | `getArrayForSamples` | function | src/imagefiledirectory.js:18 |
| 3 | `parseContentRange` | function | src/source/httputils.js:59 |
| 3 | `times` | function | src/utils.ts:65 |
| 2 | `AbortError` | class | src/utils.ts:159 |
| 2 | `BaseClient.constructor` | constructor | src/source/client/base.js:36 |
| 2 | `BaseClient.request` | method | src/source/client/base.js:45 |
| 2 | `BaseDecoder.constructor` | constructor | src/compression/basedecoder.js:17 |
| 2 | `BaseResponse.getData` | method | src/source/client/base.js:29 |
| 2 | `BaseResponse.getHeader` | method | src/source/client/base.js:22 |
| 2 | `BaseSource.fetchSlice` | method | src/source/basesource.js:26 |
| 2 | `Block` | class | src/source/blockedsource.js:5 |
| 2 | `DataSlice` | class | src/dataslice.js:1 |

**Yorum:** `BaseDecoder`/`BaseSource`/`BaseResponse` sınıf-seviyesi elemanların "giden bağımlılığı yok" görünmesi beklenen bir yapısal sonuçtur: sınıf gövdesinin kendisinde (metot gövdeleri dışında) başka bir elemana referans yoktur — asıl çağrılar `decode()`, `fetch()` gibi metot elemanlarının içinde olur ve o metotlara atfedilir, sınıfın kendisine değil.

---

## 5. Doğrulama (zorunlu adım)

### 5a. Bağımsız çapraz kontrol (grep)

| Eleman | Araç sonucu (tekil çağıran) | grep çapraz kontrol | Değerlendirme |
|---|---:|---|---|
| `ImageFileDirectory.getValue` | 19 | 30 çağrı satırı / 3 dosya | Mertebe uyumlu — geotiffimage.js'te aynı yöntemi çağıran ~16 farklı metot var |
| `GeoTIFFImage.getHeight` | 9 | 10 çağrı satırı / 2 dosya | Neredeyse birebir — iki çağrı satırı (277/279) aynı metoda ait |
| `GeoTIFFImage.getWidth` | 8 | 10 çağrı satırı / 2 dosya | Uyumlu |
| `BaseSource` | 9 | 5 `extends` + 4 JSDoc tip referansı | Bkz. §3 açıklaması — birebir örtüşüyor |

Sonuç: **mertebe (order of magnitude) tüm örneklerde tutarlı**; `getHeight` örneğinde çağıran-eleman sayısı elle sayımla birebir eşleşti.

### 5b. Kalıtım hiyerarşisi kümelenme kontrolü — düzeltmenin somut kanıtı

Aynı ada sahip metot/erişimci 15 grup halinde ≥2 sınıfta bulundu (override adayları). **11/15 grubunda ham (düzeltme öncesi) `findReferences` sonucu tüm üyelerde birebir aynıydı** — bu, spesifikasyonun öngördüğü kalıtım-birleşmesi olgusunun bu kod tabanında gerçekten var olduğunu doğruluyor. Örnek — `decodeBlock` (9 sınıfta tanımlı, `BaseDecoder`'dan miras):

| Eleman | Ham (düzeltme öncesi) | Düzeltilmiş (tekil çağıran) |
|---|---:|---:|
| `BaseDecoder.decodeBlock` | 9 | **1** |
| `DeflateDecoder.decodeBlock` | 9 | **0** |
| `JpegDecoder.decodeBlock` | 9 | **0** |
| `LercDecoder.decodeBlock` | 9 | **0** |
| `LZWDecoder.decodeBlock` | 9 | **0** |
| `PackbitsDecoder.decodeBlock` | 9 | **0** |
| `RawDecoder.decodeBlock` | 9 | **0** |
| `WebImageDecoder.decodeBlock` | 9 | **0** |
| `ZstdDecoder.decodeBlock` | 9 | **0** |

Düzeltme uygulanmasaydı, 9 alt sınıfın hepsi "9 bağımlı" ile yanlış biçimde rapora girecekti. Düzeltmeden sonra gerçek dağılım ortaya çıkıyor: alt sınıfların `decodeBlock` metotları statik olarak hiç doğrudan çağrılmıyor (çağıranlar `BaseDecoder` tipinde bir değişken üzerinden çağırıyor — bkz. sınırlama notu §6). Aynı örüntü `fetchSlice`, `get:status`, `getHeader`, `getData`, `request`, `getImage`, `fetch`, `get:fileSize` gruplarında da gözlendi (hepsinde ham→düzeltilmiş fark yarattı).

**3 grup düzeltmeden sonra da tüm üyelerde aynı (küçük) sayıyı gösterdi** — `getImageCount` (hepsi=1), `getSlice` (hepsi=1), `constructRequest` (hepsi=1). Bunlar potansiyel "düzeltme başarısız" sinyali olabileceğinden elle incelendi: her elemanın gerçek çağıranı SQL ile sorgulandı ve **her birinin farklı, gerçek bir çağırana sahip olduğu doğrulandı** (örn. `GeoTIFFBase.getImageCount ← GeoTIFFBase.readRasters`, `GeoTIFF.getImageCount ← MultiGeoTIFF.getImageCount`, `MultiGeoTIFF.getImageCount ← MultiGeoTIFF.getImage` — üç farklı çağıran, tesadüfen üçü de tekil). Sonuç: düzeltme hatası değil, düşük sayılarda rastlantısal eşitlik.

Kalan 4 grup (`close`, `get:buffer`, `readRasters`, `getSlice`ait olmayan bazı üyeler) ham sayıları farklıydı — bunlar gerçek kalıtım ilişkisi olmayan, sadece isim çakışması olan metotlardı (örn. `GeoTIFF.close` ile `BaseSource.close`/`FileSource.close` ilgisiz sınıflardır); bu da eşleştirmenin yalnızca gerçek hiyerarşilerde birleşme ürettiğini, rastgele isim çakışmasında üretmediğini gösteriyor.

### 5c. Bütünlük kontrolleri

- Yinelenen (çağıran, çağrılan) çifti: **0**
- Kendine-kenar (`caller_id = callee_id`): **0**

---

## 6. Bilinen sınırlamalar ve hata payı

1. **Statik tip / çok biçimlilik (polymorphism) sınırı:** Bir alt sınıfın override ettiği metot, çağıran taraf temel-sınıf tipinde bir değişken üzerinden çağırdığında (ör. `BaseSource` tipinde tutulan bir `source.fetch()`), derleyici çağrıyı **statik olarak** temel sınıfın bildirimine çözümler — çalışma zamanında hangi alt sınıfın çalışacağını bilemez. Bu yüzden override metotların "gerçek" bağımlı sayısı sistematik olarak düşük çıkabilir (yukarıdaki `decodeBlock`, `fetchSlice` örnekleri). Bu, spesifikasyonun beklediği ve `find-references` tabanlı statik analizin doğal bir sınırıdır; dinamik/runtime çağrı izleme olmadan giderilemez.
2. **Destructuring örüntüsü küçük bir kayıp kaynağı:** `const { fileSize } = this;` gibi 3 nesne-çözme (destructuring) örüntüsünde derleyici sembolü, kataloglanan getter bildirimiyle birebir eşleşmeyen bir ara temsile çözümledi (984 ham girdinin %0.3'ü — `BaseSource.get:fileSize` / `BlockedSource.get:fileSize` / `RemoteSource.get:fileSize`, hepsi `blockedsource.js:90`). Bu girdiler ne yanlış elemana ne de doğru elemana sayıldı; sessizce atlandı, kaybolmadı (edge oluşturulmadı).
3. **Çağıransız referanslar (160 adet, toplam ham girdinin ~%16'sı):** Import/re-export satırları ve modül üst-düzeyi (herhangi bir fonksiyon/metot/kurucu gövdesi dışı) referanslar hiçbir elemana atfedilmedi — bunlar gerçek "çağrı" değil, bağlama (binding) noktalarıdır. Bu, spesifikasyonda öngörülen ve kabul edilen bir eksikliktir.
4. **Sembolü hiç çözümlenemeyen referans:** Bu çalıştırmada **0** — geotiff.js'in JS+TS karışık, `checkJs` açık kod tabanında böyle bir durumla karşılaşılmadı. Farklı bir sürümde/derleyici versiyonunda oluşursa, spesifikasyon gereği işlenmekte olan elemana sayılacak şekilde kodlanmıştır (`analysis/tool/analyze.mjs` içindeki `unresolvedFallback` dalı).
5. **`new X()` → kurucu yönlendirmesi elle eklenmiş bir kuraldır**, derleyicinin doğal `findReferences` davranışı değildir (checker `new X()`'i sınıf sembolüne çözümler). Sınıfın açık kurucusu yoksa (örn. `BaseSource`, `GeoTIFFBase`, `BaseResponse`) referans sınıf elemanında kalır — bu da sınıf elemanlarının bağımlı sayısına gerçek örnekleme (instantiation) çağrılarının karışmasına neden olabilir (ör. `BaseResponse`'un 4 bağımlısının bir kısmı örnekleme değil, tip referansı olabilir; her sayı tek tek doğrulanmadı, sadece üst-seviye örnekler §5a'da elle kontrol edildi).

---

## 7. Tekrarlanabilirlik

- Kod: `analysis/tool/analyze.mjs` (envanter + referans + çağıran ataması + kenar), `build-db.mjs` (SQLite + iki sorgu), `validate.mjs` (§5b).
- Girdi commit: `8594d1b4bde4072326916185c848e73a9e704850`.
- Araç sürümleri: ts-morph 28.0.0, typescript 7.0.2, Node.js v22.23.1 (`node:sqlite` deneysel API).
- Farklı bir TypeScript/derleyici sürümüyle tekrar çalıştırıldığında sembol çözümlemesinde (özellikle §6.1'deki statik-tip sınırının etkilediği kenar sayılarında) küçük farklılıklar beklenmelidir; yöntemin kendisi deterministiktir.
- Yeniden çalıştırma: `analysis/README.md` içindeki adımlara bakın.
