use geotiff::{ReadRasterResult, ReadRastersOptions, ReadRgbOptions, from_file};

const DEFAULT_PATH: &str = "tests/fixtures/planar-rgb-u8.tif";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_PATH.to_owned());
    let tiff = from_file(&path).await?;
    let image = tiff.image(0)?;

    println!("file: {path}");
    println!("images: {}", tiff.image_count());
    println!("dimensions: {} x {}", image.width(), image.height());
    println!("samples per pixel: {}", image.samples_per_pixel());
    if let Ok(bbox) = image.bounding_box(false) {
        println!("bounding box: {bbox:?}");
    }

    let raster = image
        .read_rasters(ReadRastersOptions {
            interleave: true,
            ..ReadRastersOptions::default()
        })
        .await?;
    let ReadRasterResult::Interleaved(raster) = raster else {
        unreachable!("interleave=true always returns an interleaved raster");
    };
    let preview = (0..raster.data.len().min(12))
        .map(|index| raster.data.get_f64(index))
        .collect::<Vec<_>>();
    println!(
        "raster: {} x {}, {} samples, first values: {:?}",
        raster.width,
        raster.height,
        raster.data.len(),
        preview
    );

    let rgb = image
        .read_rgb(ReadRgbOptions {
            interleave: true,
            ..ReadRgbOptions::default()
        })
        .await?;
    let ReadRasterResult::Interleaved(rgb) = rgb else {
        unreachable!("interleave=true always returns an interleaved RGB raster");
    };
    println!(
        "RGB: {} x {}, {} values",
        rgb.width,
        rgb.height,
        rgb.data.len()
    );

    Ok(())
}
