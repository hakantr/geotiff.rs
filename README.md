# geotiff.rs

Türkçe | [English](https://github.com/hakantr/geotiff.rs/blob/main/README_EN.md)

[![Release](https://github.com/hakantr/geotiff.rs/actions/workflows/release.yml/badge.svg)](https://github.com/hakantr/geotiff.rs/actions/workflows/release.yml)
[![Dokümantasyon](https://github.com/hakantr/geotiff.rs/actions/workflows/deploy.yml/badge.svg)](https://github.com/hakantr/geotiff.rs/actions/workflows/deploy.yml)

TIFF/GeoTIFF raster verilerini JavaScript veya tarayıcı çalışma zamanı olmadan
okumak, çözmek, dönüştürmek ve yazmak için geliştirilmiş, asenkron ve native bir
[geotiff.js 3.1.0](https://github.com/geotiffjs/geotiff.js/tree/v3.1.0) Rust
portudur.

Crate sürümü bilinçli olarak upstream davranış ve API tabanıyla birebir
`3.1.0` değerine sabitlenmiştir. Canlı differential testlerde geotiff.js'in
`8594d1b4bde4072326916185c848e73a9e704850` commit'i kullanılır. Eksiksiz
uyumluluk sözleşmesi, doğrulanmış farklar ve kabul kanıtları
[PORTING_PLAN.md](https://github.com/hakantr/geotiff.rs/blob/main/PORTING_PLAN.md)
içinde kayıtlıdır.

Bu README'deki Mount Yasur SAR önizlemesi, sabitlenmiş gerçek GeoTIFF'in bu Rust
portuyla açılması, `read_rgb` ile çözülmesi ve bilinear yöntemle 800 x 800'e
yeniden örneklenmesiyle üretilmiştir. PNG yeniden açılıp bütün pikselleri
doğrulanmıştır. Durum rozetleri de bu reponun kendi workflow'larına aittir;
geotiff.js README'sindeki hiçbir görsel varlık kullanılmaz.

![geotiff.rs tarafından üretilen Mount Yasur SAR GeoTIFF çıktısı](https://raw.githubusercontent.com/hakantr/geotiff.rs/main/docs/assets/mount-yasur-sar.png)

_Mount Yasur SAR görüntüsü: [Umbra Space Open Data](https://umbra.space/open-data),
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). Sabitlenmiş
[kaynak GeoTIFF](https://github.com/GeoTIFF/test-data/blob/8506204783ff26a6c49ed1f721e7e1635b2e43ee/files/umbra_mount_yasur.tiff),
[üretici/doğrulayıcı örnek](https://github.com/hakantr/geotiff.rs/blob/main/examples/render_png.rs)
ve [ayrıntılı atıf kaydı](https://github.com/hakantr/geotiff.rs/blob/main/docs/assets/README.md)._

## Özellikler

- Yerel dosya, bellek, HTTP/HTTPS range kaynağı, özel asenkron reader ve
  S3/GCS/Azure/yerel `object_store` uygulamalarından GeoTIFF açma.
- Ana görüntüyü bir veya daha fazla harici overview dosyasıyla birleştirme.
- Classic TIFF ve BigTIFF'i her iki byte sırasında okuma; native 64 bit
  tamsayıları `f64` üzerinden daraltmadan koruma.
- Striped veya tiled görüntüleri chunky ve planar configuration ile okuma.
- İşaretli/işaretsiz 1–64 bit tamsayılar ile Float16/32/64 örnekleri okuma.
  Packed sample verileri için kayıpsız native ve tam geotiff.js uyumluluk
  modları.
- Bant ve pencere seçimi, görüntü dışı alan doldurma, ayrı bant veya
  pixel-interleaved çıktı ve nearest/bilinear yeniden örnekleme.
- İstenen çıktı çözünürlüğü ya da bounding box için dahili/harici overview
  seviyesini otomatik seçme.
- WhiteIsZero, BlackIsZero, RGB, palette, CMYK, YCbCr ve CIELab verilerini,
  desteklenen alpha sample'larıyla birlikte RGB'ye dönüştürme.
- Uncompressed, PackBits, LZW, Deflate, JPEG, LERC, Zstandard ve WebP çözme.
  JPEG2000 ek bir native yetenek olarak sunulur.
- Decoder kaydetme/override etme, sıkıştırılmış/çözülmüş cache ayarlama, decode
  paralelliğini sınırlama ve I/O/decode iptali.
- GeoKey, GDAL metadata/NoData, COG ghost metadata, tie point, origin,
  resolution, PixelIsArea ve bounding box erişimi.
- Düz typed değerlerden veya iç içe bantlardan multi-strip, tiled, planar,
  GeoKey ve GDAL metadata içeren Classic GeoTIFF yazma.

## Kurulum

crates.io üzerindeki `geotiff` paket adı bu projeyle ilgisi olmayan başka bir
projeye aittir. Bu nedenle port yanlış registry kimliği altında yayınlanmaz.
GitHub release tag'ini `Cargo.toml` içinde sabitleyin:

```toml
[dependencies]
geotiff = { git = "https://github.com/hakantr/geotiff.rs", tag = "v3.1.0" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Native codec bağımlılıkları crate ile birlikte derlenir. Hedef platformda
standart Rust toolchain'e ek olarak C/C++ derleyicisi, CMake ve libclang
bulunmalıdır.

## Doğrulanmış kullanım örnekleri

Aşağıdaki örnekler varsayımsal API taslakları değildir. Aynı kaynaklar
`examples/` altında bulunur; release kapısında derlenir ve bu sürüm hazırlanırken
repo fixture'ları veya sabitlenmiş gerçek uzak dosya üzerinde çalıştırılmıştır.

### Yerel GeoTIFF açma, raster ve RGB okuma

Bu örnek repodaki gerçek planar RGB fixture'ını açar, interleaved raster ve RGB
çıktısını okur:

```rust,no_run
use geotiff::{ReadRasterResult, ReadRastersOptions, ReadRgbOptions, from_file};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tiff = from_file("tests/fixtures/planar-rgb-u8.tif").await?;
    let image = tiff.image(0)?;

    println!("{} x {}", image.width(), image.height());
    println!("sample sayısı: {}", image.samples_per_pixel());

    let raster = image
        .read_rasters(ReadRastersOptions {
            interleave: true,
            ..ReadRastersOptions::default()
        })
        .await?;
    let ReadRasterResult::Interleaved(raster) = raster else {
        unreachable!("interleave=true her zaman interleaved raster döndürür");
    };
    println!("raster değer sayısı: {}", raster.data.len());

    let rgb = image
        .read_rgb(ReadRgbOptions {
            interleave: true,
            ..ReadRgbOptions::default()
        })
        .await?;
    let ReadRasterResult::Interleaved(rgb) = rgb else {
        unreachable!("interleave=true her zaman interleaved RGB döndürür");
    };
    println!("RGB: {} x {}, {} değer", rgb.width, rgb.height, rgb.data.len());
    Ok(())
}
```

Çalıştırılabilir tam sürüm:
[examples/read_file.rs](https://github.com/hakantr/geotiff.rs/blob/main/examples/read_file.rs)

```sh
cargo run --locked --example read_file
```

Yukarıdaki Mount Yasur SAR GeoTIFF'ini sabit corpus URL'sinden açmak, portun
BlackIsZero→RGB ve bilinear resampling yollarıyla 800 x 800 PNG üretmek ve bütün
pikselleri yeniden doğrulamak için:

```sh
cargo run --locked --example render_png
```

### Gerçek URL'den GeoTIFF okuma

Örnek URL, `GeoTIFF/test-data` deposundaki commit'i ve dosyayı sabitler:

```rust,no_run
use geotiff::{HttpSourceOptions, ReadRasterResult, ReadRastersOptions, from_url_with_options};

const URL: &str = concat!(
    "https://raw.githubusercontent.com/GeoTIFF/test-data/",
    "8506204783ff26a6c49ed1f721e7e1635b2e43ee/",
    "files/GeogToWGS84GeoKey5.tif"
);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tiff = from_url_with_options(
        URL,
        HttpSourceOptions {
            allow_full_file: true,
            ..HttpSourceOptions::default()
        },
    )
    .await?;
    let image = tiff.image(0)?;
    let raster = image
        .read_rasters(ReadRastersOptions {
            interleave: true,
            ..ReadRastersOptions::default()
        })
        .await?;
    let ReadRasterResult::Interleaved(raster) = raster else {
        unreachable!("interleave=true her zaman interleaved raster döndürür");
    };

    println!("GeoKeys: {:?}", image.geo_keys());
    println!("bounding box: {:?}", image.bounding_box(false)?);
    println!("çözülen değer sayısı: {}", raster.data.len());
    Ok(())
}
```

Çalıştırılabilir kaynak:
[examples/read_remote.rs](https://github.com/hakantr/geotiff.rs/blob/main/examples/read_remote.rs)

```sh
cargo run --locked --example read_remote
```

### Pencere, bant seçimi ve yeniden örnekleme

Bu örnek gerçek 12 bit RGB fixture'ın sol üst penceresinden sırasıyla 2. ve 0.
bantları seçer ve sonucu bilinear yöntemle 16 x 16 boyutuna getirir:

```rust,no_run
use geotiff::{ImageWindow, ReadRastersOptions, from_file};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tiff = from_file("tests/fixtures/12bit.cropped.rgb.tiff").await?;
    let image = tiff.image(0)?;
    let window = ImageWindow {
        x0: 0,
        y0: 0,
        x1: image.width().min(32) as i64,
        y1: image.height().min(32) as i64,
    };
    let raster = image
        .read_rasters(ReadRastersOptions {
            window: Some(window),
            samples: vec![2, 0],
            interleave: true,
            width: Some(16),
            height: Some(16),
            resample_method: "bilinear".to_owned(),
            ..ReadRastersOptions::default()
        })
        .await?;
    assert_eq!((raster.width(), raster.height()), (16, 16));
    Ok(())
}
```

Çalıştırılabilir kaynak:
[examples/read_window.rs](https://github.com/hakantr/geotiff.rs/blob/main/examples/read_window.rs)

```sh
cargo run --locked --example read_window
```

Packed sample verilerinde varsayılan `PackedSampleMode::Lossless` bütün
örnekleri korur. Geçiş sırasında 3.1.0'ın packed-offset davranışını birebir
üretmek gerekiyorsa `PackedSampleMode::GeotiffJs` seçilebilir.

### GeoTIFF yazma ve yeniden okuyarak doğrulama

Bu örnek 2 x 2 bir GeoTIFF yazar, aynı dosyayı port ile yeniden açar ve raster
değerlerini doğrular:

```rust,no_run
use geotiff::{
    ReadRasterResult, ReadRastersOptions, TypedArray, WriterMetadata, from_file,
    write_array_buffer,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::temp_dir().join("geotiff-rs-example.tif");
    let pixels = vec![1_u8, 2, 3, 4];
    let bytes = write_array_buffer(pixels.clone(), WriterMetadata::new(2, 2))?;
    std::fs::write(&output, bytes)?;

    let tiff = from_file(&output).await?;
    let raster = tiff
        .image(0)?
        .read_rasters(ReadRastersOptions {
            interleave: true,
            ..ReadRastersOptions::default()
        })
        .await?;
    let ReadRasterResult::Interleaved(raster) = raster else {
        unreachable!("interleave=true her zaman interleaved raster döndürür");
    };
    assert_eq!(raster.data, TypedArray::Uint8(pixels));
    Ok(())
}
```

Çalıştırılabilir kaynak:
[examples/write_geotiff.rs](https://github.com/hakantr/geotiff.rs/blob/main/examples/write_geotiff.rs)

```sh
cargo run --locked --example write_geotiff
```

Writer varsayılan olarak kayıpsız native serialization yapar. Yalnız JavaScript
3.1.0 writer'ının signed-array wire davranışının aynısı gerektiğinde
`write_array_buffer_with_mode(..., WriterCompatibility::GeotiffJs)` kullanın.

## Metadata erişimi

Bir `GeoTiffImage` üzerinden şu metadata yolları doğrudan erişilebilirdir:

```rust,no_run
# async fn metadata(image: &geotiff::GeoTiffImage<'_>) -> Result<(), Box<dyn std::error::Error>> {
println!("GeoKeys: {:?}", image.geo_keys());
println!("tie points: {:?}", image.tie_points()?);
println!("GDAL metadata: {:?}", image.gdal_metadata(None)?);
println!("GDAL NoData: {:?}", image.gdal_nodata());
println!("origin: {:?}", image.origin()?);
println!("resolution: {:?}", image.resolution(None)?);
println!("pixel is area: {}", image.pixel_is_area());
# Ok(())
# }
```

Sayısal veya isimli TIFF tag erişimi `image.file_directory()` üzerinden
yapılır. geotiff.js'in deferred array modelinden farklı olarak native directory,
dosya açıldıktan sonra eager biçimde actualize edilmiştir.

## JavaScript–Rust API eşlemesi

| geotiff.js | geotiff.rs |
|---|---|
| `fromUrl`, `fromArrayBuffer`, `fromBlob`, `fromFile` | `from_url`, `from_array_buffer`, `from_blob`, `from_file` |
| `fromCustomClient` | `from_custom_client`, `from_reader` |
| `fromUrls` | `from_urls` |
| `GeoTIFF`, `MultiGeoTIFF` | `SingleGeoTiff`, `MultiGeoTiff` |
| `GeoTIFFBase` | `GeoTiffDataset` |
| `GeoTIFFImage` | `GeoTiffImage` |
| `readRasters`, `readRGB` | `read_rasters`, `read_rgb` |
| `AbortSignal` | `CancellationToken` |
| `Pool` | native Rayon decode pool |
| `addDecoder`, `getDecoder` | `add_decoder`, `get_decoder` |
| `writeArrayBuffer`, `writeGeotiff` | `write_array_buffer`, `write_geotiff` |

Rust'ın açık result, enum ve ownership tipleri JavaScript'in dinamik nesnelerinin
yerini alırken geçerli girdi/çıktı davranışını korur. Güvenlik veya veri koruma
amacıyla bilinçli bırakılan farklar
[PORTING_PLAN.md](https://github.com/hakantr/geotiff.rs/blob/main/PORTING_PLAN.md)
içinde listelenir.

## Test ve derleme

```sh
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo test --locked --all-targets
cargo test --locked --doc
cargo clippy --locked --all-targets -- -D warnings
cargo doc --locked --no-deps
```

Test paketi; unit/integration fixture'larına ek olarak aynı işlemleri bu repo ve
sibling geotiff.js 3.1.0 reposunda gerçekten çalıştıran differential oracle'lar
içerir. Komutlar ve sabitlenmiş
[GeoTIFF/test-data](https://github.com/GeoTIFF/test-data) corpus kapısı için
[differential test rehberine](https://github.com/hakantr/geotiff.rs/blob/main/tests/differential/README.md)
bakın.

## Release ve dokümantasyon

- Tam `v3.1.0` tag'i push edildiğinde release workflow; tag, Cargo sürümü,
  geotiff.js referans sürümü, temiz JS build'i, beş canlı JS/Rust differential
  matrisi, sabitlenmiş `GeoTIFF/test-data` corpus'u, format, Clippy, doctest'ler,
  Rustdoc ve crate paketini doğrular. Ardından `.crate` dosyası ile SHA-256
  checksum'unu içeren GitHub Release oluşturur.
- `main` branch'ine her push ve manuel dispatch, Rustdoc üretip GitHub Pages'e
  deploy eder. İlk deploy öncesinde repo Pages kaynağı **GitHub Actions** olarak
  ayarlanmalıdır.

crates.io upload yapılmaz: `geotiff` registry adı ilgisiz `georust/geotiff`
projesine aittir. Bu koruma, portun başka bir projenin paket kimliği altında
yayınlanmasını engeller.

## Kapsam ve bilinen sınır

Koordinat reprojection, harita renderer'ı ve GPU texture yönetimi geotiff.js'in
ve bu portun kapsamı dışındadır. Native WebP RGB/RGBA decode testleri geçer;
sertifikasyondaki tek açık madde aynı WebP sonucunun gerçek tarayıcı
`createImageBitmap` backend'iyle karşılaştırılmasıdır ve port planında açıkça
kayıtlıdır.

## Katkı

Issue ve pull request'ler kabul edilir. Gözlenebilir davranışı etkileyen
değişiklikler hem Rust regresyon testi hem de uygun olduğunda canlı geotiff.js
differential case'i eklemelidir. Yeni bilinçli farklar differential ledger'da
isimlendirilip gerekçelendirilmelidir.

## Lisans ve teşekkür

[MIT Lisansı](https://github.com/hakantr/geotiff.rs/blob/main/LICENSE) ile
lisanslanmıştır. Bu port upstream geotiff.js lisansını ve attribution bilgisini
korur. geotiff.js, EOX IT Services GmbH ve katkıcıları tarafından geliştirilmiş;
bu native uygulama onun API tasarımını ve doğrulanmış davranışını temel almıştır.
