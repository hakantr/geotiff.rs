use geotiff::{ImageWindow, ReadRastersOptions, from_file};

const DEFAULT_PATH: &str = "tests/fixtures/12bit.cropped.rgb.tiff";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_PATH.to_owned());
    let tiff = from_file(&path).await?;
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

    println!(
        "window {window:?} -> {} x {}",
        raster.width(),
        raster.height()
    );
    Ok(())
}
