# Map assets

`earth_surface.png` is a 4096x2048 derivative of the public-domain
[Natural Earth II](https://www.naturalearthdata.com/downloads/10m-raster-data/10m-natural-earth-2/)
`NE2_LR_LC` raster. It was resized from the source GeoTIFF without changing
the equirectangular projection or longitude origin.

`earth_mask.png` is the matching equirectangular land/ocean mask used to keep
theme ocean colors independent from the terrain texture.

`earth_lights.png` is a single-channel city-lights texture derived from NASA's
public-domain [Earth at Night / Black Marble](https://earthobservatory.nasa.gov/features/NightLights)
composite. The source luminance was thresholded to drop the faint landmass haze
and keep the city lights, then stored equirectangular (same projection/origin).
It drives the golden night-time city glow in the dark theme.

`night_bkg.png` is a procedurally generated starfield used as the dark-theme
window background.
