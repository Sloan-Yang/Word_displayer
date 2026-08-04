# Map assets

`earth_surface.png` is a 4096x2048 derivative of the public-domain
[Natural Earth II](https://www.naturalearthdata.com/downloads/10m-raster-data/10m-natural-earth-2/)
`NE2_LR_LC` raster. It was resized from the source GeoTIFF without changing
the equirectangular projection or longitude origin.

`earth_mask.png` is the matching equirectangular land/ocean mask used to keep
theme ocean colors independent from the terrain texture.

## Application icon

`app_icon_source.svg` is derived from Font Awesome Free's `book-atlas` icon,
copyright Fonticons, Inc. and licensed under
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). The original icon
and license are available from
[Font Awesome](https://fontawesome.com/icons/classic/solid/book-atlas).

`app_icon.png` is the 512x512 runtime icon. `app_icon.ico` contains
256, 128, 64, 48, 32, 24, and 16 pixel variants for the Windows executable.

`earth_lights.png` is a single-channel city-lights texture derived from NASA's
public-domain [Earth at Night / Black Marble](https://earthobservatory.nasa.gov/features/NightLights)
composite. The source luminance was thresholded to drop the faint landmass haze
and keep the city lights, then stored equirectangular (same projection/origin).
It drives the golden night-time city glow in the dark theme.

`night_bkg.png` is a procedurally generated starfield used as the dark-theme
window background.
