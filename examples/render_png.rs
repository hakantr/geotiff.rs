use geotiff::{
    HttpSourceOptions, ReadRasterResult, ReadRgbOptions, TypedArray, from_file,
    from_url_with_options,
};
use image::{ColorType, ImageFormat};

const DEFAULT_INPUT: &str = concat!(
    "https://raw.githubusercontent.com/GeoTIFF/test-data/",
    "8506204783ff26a6c49ed1f721e7e1635b2e43ee/",
    "files/umbra_mount_yasur.tiff"
);
const DEFAULT_OUTPUT: &str = "docs/assets/mount-yasur-sar.png";
const DEFAULT_SIZE: usize = 800;

fn parse_dimension(
    value: Option<String>,
    fallback: Option<usize>,
    name: &str,
) -> Result<Option<usize>, std::io::Error> {
    let Some(value) = value else {
        return Ok(fallback);
    };
    let parsed = value.parse::<usize>().map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid {name} {value:?}: {error}"),
        )
    })?;
    if parsed == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name} must be greater than zero"),
        ));
    }
    Ok(Some(parsed))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let supplied_input = args.next();
    let uses_default_input = supplied_input.is_none();
    let input = supplied_input.unwrap_or_else(|| DEFAULT_INPUT.to_owned());
    let output = args.next().unwrap_or_else(|| DEFAULT_OUTPUT.to_owned());
    let default_dimension = uses_default_input.then_some(DEFAULT_SIZE);
    let width = parse_dimension(args.next(), default_dimension, "width")?;
    let height = parse_dimension(args.next(), default_dimension, "height")?;
    if args.next().is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "usage: render_png [INPUT_OR_URL] [OUTPUT] [WIDTH] [HEIGHT]",
        )
        .into());
    }

    let tiff = if input.starts_with("http://") || input.starts_with("https://") {
        from_url_with_options(
            &input,
            HttpSourceOptions {
                allow_full_file: true,
                ..HttpSourceOptions::default()
            },
        )
        .await?
    } else {
        from_file(&input).await?
    };
    let image = tiff.image(0)?;
    let bounding_box = image.bounding_box(false).ok();
    let rgb = image
        .read_rgb(ReadRgbOptions {
            interleave: true,
            width,
            height,
            resample_method: "bilinear".to_owned(),
            enable_alpha: false,
            ..ReadRgbOptions::default()
        })
        .await?;
    let ReadRasterResult::Interleaved(rgb) = rgb else {
        unreachable!("interleave=true always returns an interleaved RGB raster");
    };
    let (TypedArray::Uint8(pixels) | TypedArray::Uint8Clamped(pixels)) = rgb.data else {
        return Err(std::io::Error::other("source did not produce 8-bit RGB").into());
    };
    if pixels.len() != rgb.width * rgb.height * 3 {
        return Err(std::io::Error::other("source did not produce three RGB channels").into());
    }

    if let Some(parent) = std::path::Path::new(&output)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    if pixels
        .chunks_exact(3)
        .all(|pixel| pixel[0] == pixel[1] && pixel[1] == pixel[2])
    {
        let grayscale = pixels
            .chunks_exact(3)
            .map(|pixel| pixel[0])
            .collect::<Vec<_>>();
        image::save_buffer_with_format(
            &output,
            &grayscale,
            rgb.width as u32,
            rgb.height as u32,
            ColorType::L8,
            ImageFormat::Png,
        )?;
    } else {
        image::save_buffer_with_format(
            &output,
            &pixels,
            rgb.width as u32,
            rgb.height as u32,
            ColorType::Rgb8,
            ImageFormat::Png,
        )?;
    }

    // Decode the generated PNG independently and compare every RGB value;
    // grayscale PNGs are expanded back to the original three equal channels.
    let reopened = image::open(&output)?.into_rgb8();
    assert_eq!(reopened.dimensions(), (rgb.width as u32, rgb.height as u32));
    assert_eq!(reopened.as_raw(), &pixels);

    if let Some(bounding_box) = bounding_box {
        println!("source bounding box: {bounding_box:?}");
    }
    println!(
        "rendered and verified: {input} -> {output} ({} x {})",
        rgb.width, rgb.height
    );
    Ok(())
}
