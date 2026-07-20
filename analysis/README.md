# geotiff.js Bağımlılık Analizi

Bu klasör, `../geotiff.js` reposunun (bu repo `geotiff.rs` ile kardeş dizinde) iki ayrı bağımlılık
analizinin sonuçlarını içerir:

- **A. İç bağımlılık analizi** — eleman düzeyinde (fonksiyon/sınıf/metot/kurucu) "kim kimi
  çağırıyor" grafiği, derleyici sembol çözümlemesiyle (metin araması değil) çıkarıldı.
- **B. Dış bağımlılık listesi** — `src/`'in kullandığı npm paketleri, `package.json` ile
  çapraz doğrulandı.

## Sonuçlar

| Dosya | İçerik |
|---|---|
| [`output/A-internal-bagimlilik-analizi.md`](output/A-internal-bagimlilik-analizi.md) | İç bağımlılık raporu: yöntem, envanter, top-25 sorguları, doğrulama, sınırlamalar |
| [`output/B-dis-bagimlilik-listesi.md`](output/B-dis-bagimlilik-listesi.md) | Dış bağımlılık raporu: yöntem, paket tablosu, ölü/hayalet bağımlılık bulguları |

## Ham veri

| Dosya | İçerik |
|---|---|
| `data/elements.json` | 329 kataloglanmış eleman (kimlik, tür, ad, dosya, satır, ham grup boyutu) |
| `data/edges.json` | 421 tekil (çağıran→çağrılan) kenar (`occurrences`, `fallback` meta verisiyle) |
| `data/stats.json` | A.2-3 adımının işlem istatistikleri (atlanan/çağıransız/vb. sayılar) |
| `data/analysis.sqlite` | `elements` + `edges` tabloları — serbestçe sorgulanabilir (`node:sqlite`) |
| `data/query-top-depended-upon.json` | Sorgu 1 sonucu (top 25) |
| `data/query-leaf-but-foundational.json` | Sorgu 2 sonucu (top 25) |
| `data/integrity-checks.json` | §5c bütünlük kontrolleri |
| `data/validation-override-groups.json` | §5b kalıtım-kümelenme doğrulama verisi |
| `data/external-deps.json` | Bölüm B'nin ham çıktısı (paket → dosyalar/bindings/sınıflandırma) |

## Kapsam ve tekrarlanabilirlik

- **Analiz edilen dizin:** `../geotiff.js/src/` (36 dosya: 34 `.js`, 2 `.ts`; `test/` dahil değil —
  `tsconfig.json`'ın `include`'u zaten sadece `src/`'i kapsıyor).
- **Commit:** `8594d1b4bde4072326916185c848e73a9e704850` (2026-05-26).
- **Analiz tarihi:** 2026-07-19.
- **Araçlar:** ts-morph 28.0.0 (typescript 7.0.2 derleyici API'si üzerinde), Node.js v22.23.1,
  `node:sqlite` (deneysel API, `--experimental-sqlite` bayrağı bu Node sürümünde gerekmiyor).
- Yöntem deterministiktir ama **farklı bir TypeScript/derleyici sürümü sembol çözümlemesinde
  tekil farklar üretebilir** (özellikle A raporunun §6'sındaki statik-tip/polymorphism sınırının
  etkilediği kenarlarda). Bu yüzden commit ve araç sürümleri burada birlikte kaydedildi.

## Nasıl tekrar çalıştırılır

```sh
cd analysis/tool
npm install                 # ts-morph + typescript (bir kere)

# geotiff.js'in kendi bağımlılıklarının kurulu olması gerekir (tip çözümlemesi için):
(cd ../../../geotiff.js && npm ci)

node analyze.mjs             # A.1-3: envanter + referans toplama + çağıran ataması -> data/*.json
node build-db.mjs            # A.4: SQLite + iki sorgu -> data/analysis.sqlite, query-*.json
node validate.mjs            # A.5b: kalıtım-kümelenme doğrulaması -> data/validation-override-groups.json
node external-deps.mjs       # B: dış bağımlılık çıkarımı -> data/external-deps.json
```

`REPO_ROOT` sabiti her script'in başında `../geotiff.js`'in mutlak yoluna ayarlı; repo başka bir
yere taşınırsa bu satırlar güncellenmeli.

### Veritabanını elle sorgulama örneği

```js
import { DatabaseSync } from "node:sqlite";
const db = new DatabaseSync("analysis/data/analysis.sqlite", { readOnly: true });
db.prepare(`
  SELECT el.name, COUNT(DISTINCT ed.caller_id) AS dependents
  FROM elements el JOIN edges ed ON ed.callee_id = el.id
  WHERE el.kind = 'method'
  GROUP BY el.id ORDER BY dependents DESC LIMIT 10
`).all();
```

## Kod haritası (`tool/`)

- `analyze.mjs` — A.1 (eleman envanteri) + A.2-3 (referans toplama, kalıtım-birleşmesi düzeltmesi,
  çağıran ataması, kenar oluşturma).
- `build-db.mjs` — A.4 (SQLite yükleme + iki kanonik sorgu) + A.5c (bütünlük kontrolleri).
- `validate.mjs` — A.5b (override-kümelenme, ham/düzeltilmiş karşılaştırması).
- `external-deps.mjs` — B (AST tabanlı import çıkarımı, manifest karşılaştırması, sınıflandırma).

A.5a (grep çapraz kontrolü) elle yapıldı; sonuçları A raporunun §5a'sında belgelendi.
