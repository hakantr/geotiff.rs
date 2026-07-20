# Upstream Bildirimi (Taslak) — `async-tiff`'in `TokioReader`'ında bayt okuma hatası

**Durum:** Teknik olarak yeniden doğrulandı (2026-07-20); GitHub'a henüz açılmadı. Bu hata
**geotiff.js 3.1.0'ın build'iyle ilgili değildir**. Referans JS reposunda temiz `npm ci
--ignore-scripts && npm run build` başarıyla tamamlanmıştır. Buradaki bulgu yalnızca Rust
portunda kullanılan `async-tiff 0.3.0` bağımlılığının `TokioReader` uygulamasına aittir.

Bağımlılığın kurulu kaynak kodu tekrar incelendi: `Vec::with_capacity(to_read)` sonrasında
`AsyncReadExt::read(&mut buffer)` çağrısı aynen duruyor; kullanılan Tokio imzası `&mut [u8]`
aldığından sıfır uzunluklu `Vec` sıfır uzunluklu dilime dönüşüyor. Bu, daha önce gözlenen
`EndOfFile(18, 0)` sonucuyla aynı kök nedendir. Taslağın tam metni burada — birlikte gözden
geçirildikten sonra
[developmentseed/async-tiff](https://github.com/developmentseed/async-tiff) reposuna issue olarak
açılabilir.

**Nerede bulundu:** `src/source/reader.rs`'te `SourceSpec::File` için yerel dosya okuyucusu
yazarken — `async_tiff::reader::TokioReader<tokio::fs::File>` kullanan ilk testimiz
(`opens_a_local_file_via_tokio_reader`) `EndOfFile(18, 0)` hatasıyla başarısız oldu. Kaynağı
okuyup teyit ettikten sonra kendi `LocalFileReader`'ımızı yazıp `src/source/reader.rs`'e koyduk
(aynı `AsyncFileReader` trait'ini doğru şekilde uyguluyor); geotiff.rs artık bu hataya bağımlı
değil, ama upstream'de gerçek bir hata olarak duruyor ve muhtemelen `TokioReader` kullanan herkesi
etkiliyor.

---

## Issue taslağı (İngilizce, doğrudan GitHub'a yapıştırılabilir)

### Title

`TokioReader::get_bytes` always fails with `EndOfFile` — reads into an empty `Vec` instead of a
sized buffer

### Crate / version

`async-tiff = "0.3.0"`, `tokio` feature enabled.

### Description

`TokioReader::make_range_request` (`src/reader.rs`) allocates its read buffer with
`Vec::with_capacity(to_read)`, which reserves *capacity* but leaves the `Vec`'s **length** at 0.
`AsyncReadExt::read` fills the buffer's current length (i.e. an empty slice `&mut buffer[..]`),
not its capacity, so it reads 0 bytes on every call. The subsequent length check
(`read != to_read`) then always fails, so `TokioReader::get_bytes` (and therefore
`AsyncFileReader::get_bytes` for any `TokioReader<T>`, including the common
`TokioReader<tokio::fs::File>` case for local files) **always returns
`AsyncTiffError::EndOfFile(n, 0)`**, regardless of the actual file contents.

```rust
// src/reader.rs, TokioReader::make_range_request
let to_read = range.end - range.start;
let mut buffer = Vec::with_capacity(to_read as usize); // <-- len() is still 0 here
let read = file.read(&mut buffer).await? as u64;       // <-- reads into an empty slice, so `read` is always 0
if read != to_read {
    return Err(AsyncTiffError::EndOfFile(to_read, read));
}
```

### Reproduction

```rust
use async_tiff::reader::{AsyncFileReader, TokioReader};

#[tokio::main]
async fn main() {
    let path = std::env::temp_dir().join("async-tiff-repro.bin");
    tokio::fs::write(&path, b"GEOTIFF-TEST-BYTES").await.unwrap();

    let file = tokio::fs::File::open(&path).await.unwrap();
    let reader = TokioReader::new(file);

    // Expected: Ok(Bytes) containing the first 18 bytes.
    // Actual:   Err(AsyncTiffError::EndOfFile(18, 0))
    let result = reader.get_bytes(0..18).await;
    println!("{result:?}");
}
```

### Suggested fix

Give the buffer an actual length before reading into it, e.g.:

```rust
let mut buffer = vec![0u8; to_read as usize];
file.read_exact(&mut buffer).await?;
Ok(buffer.into())
```

(`read_exact` also subsumes the manual `read != to_read` length check and its `EndOfFile` error,
since it already returns an `UnexpectedEof` `io::Error` — which `AsyncTiffError` already converts
via its `#[from] std::io::Error` `IOError` variant.)

### Impact

Any caller relying on `TokioReader` (e.g. for local-file COG reading, per the crate's own docs
recommending `TokioReader<tokio::fs::File>`) will see every single `get_bytes`/`get_byte_ranges`
call fail. `ReqwestReader` and `ObjectReader` are unaffected (different implementations).

### How we found it / worked around it

While integrating `async-tiff` into a native Rust GeoTIFF port, we wrote an integration test
against a real temp file using
`TokioReader<tokio::fs::File>` and got `EndOfFile(18, 0)` on an 18-byte file. Reading
`make_range_request`'s source confirmed the `Vec::with_capacity` vs. buffer-length issue above. We
worked around it locally with our own `AsyncFileReader` implementation
(`geotiff.rs/src/source/reader.rs::LocalFileReader`) using `read_exact` on a properly sized
buffer, so this doesn't block us — filing this so upstream users relying on `TokioReader` aren't
surprised by it.
