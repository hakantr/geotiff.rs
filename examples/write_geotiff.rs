use geotiff::{
    ReadRasterResult, ReadRastersOptions, TypedArray, WriterMetadata, from_file, write_array_buffer,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args()
        .nth(1)
        .map(Into::into)
        .unwrap_or_else(|| std::env::temp_dir().join("geotiff-rs-example.tif"));
    let pixels = vec![1_u8, 2, 3, 4];
    let bytes = write_array_buffer(pixels.clone(), WriterMetadata::new(2, 2))?;
    std::fs::write(&output, bytes)?;

    // Reopen what was written and verify the observable raster, so this
    // example doubles as an executable writer round-trip.
    let tiff = from_file(&output).await?;
    let raster = tiff
        .image(0)?
        .read_rasters(ReadRastersOptions {
            interleave: true,
            ..ReadRastersOptions::default()
        })
        .await?;
    let ReadRasterResult::Interleaved(raster) = raster else {
        unreachable!("interleave=true always returns an interleaved raster");
    };
    assert_eq!(raster.data, TypedArray::Uint8(pixels));

    println!("wrote and verified: {}", output.display());
    Ok(())
}
