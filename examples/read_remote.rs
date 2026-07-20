use geotiff::{HttpSourceOptions, ReadRasterResult, ReadRastersOptions, from_url_with_options};

const TEST_DATA_URL: &str = concat!(
    "https://raw.githubusercontent.com/GeoTIFF/test-data/",
    "8506204783ff26a6c49ed1f721e7e1635b2e43ee/",
    "files/GeogToWGS84GeoKey5.tif"
);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tiff = from_url_with_options(
        TEST_DATA_URL,
        HttpSourceOptions {
            // raw.githubusercontent.com may answer a small range request with
            // the complete 4 KiB fixture; accepting that response is explicit.
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
        unreachable!("interleave=true always returns an interleaved raster");
    };

    println!("URL: {TEST_DATA_URL}");
    println!("dimensions: {} x {}", raster.width, raster.height);
    println!("GeoKeys: {:?}", image.geo_keys());
    println!("bounding box: {:?}", image.bounding_box(false)?);
    println!("decoded values: {}", raster.data.len());

    Ok(())
}
