#!/bin/sh
# Regenerate the decoder fixtures. Needs ImageMagick 7, cjxl, avifenc, heif-enc and exiftool.
# Every image is the same 16×12 picture: red, green, blue and white quadrants (top-left,
# top-right, bottom-left, bottom-right), so tests can check orientation by sampling corners.
set -eu
cd "$(dirname "$0")"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

magick -size 8x6 xc:red xc:lime +append "$tmp/top.png"
magick -size 8x6 xc:blue xc:white +append "$tmp/bottom.png"
magick "$tmp/top.png" "$tmp/bottom.png" -append -strip PNG24:"$tmp/base.png"

cp "$tmp/base.png" quadrants.png
magick "$tmp/base.png" -quality 95 quadrants.jpg
magick "$tmp/base.png" quadrants.bmp
magick "$tmp/base.png" quadrants.tga
magick "$tmp/base.png" quadrants.qoi
magick "$tmp/base.png" quadrants.ppm
magick "$tmp/base.png" -define webp:lossless=true quadrants.webp
cjxl -q 100 "$tmp/base.png" quadrants.jxl >/dev/null 2>&1
avifenc -q 100 -y 444 "$tmp/base.png" quadrants.avif >/dev/null
heif-enc -q 100 -p chroma=444 "$tmp/base.png" -o quadrants.heic >/dev/null
# Float data: linear light, values within 0..1, so it is only sRGB-encoded.
magick "$tmp/base.png" -colorspace RGB -define quantum:format=floating-point quadrants.exr

# EXIF orientations 1–8: pixels stored so that applying the tag gives the base picture.
i=1
for op in "" "-flop" "-rotate 180" "-flip" "-transpose" "-rotate -90" "-transverse" "-rotate 90"; do
    # shellcheck disable=SC2086
    magick "$tmp/base.png" $op -quality 95 "orient-$i.jpg"
    exiftool -q -overwrite_original -n -Orientation=$i "orient-$i.jpg"
    i=$((i + 1))
done

# Animations: three frames, red, lime, blue, 50/100/150 ms (APNG goes through ffmpeg, which
# keeps only the first delay).
magick -size 16x12 -delay 5 xc:red -delay 10 xc:lime -delay 15 xc:blue -loop 0 anim.gif
magick -size 16x12 -delay 5 xc:red -delay 10 xc:lime -delay 15 xc:blue -loop 0 \
    -define webp:lossless=true anim.webp
magick -size 16x12 -delay 5 xc:red -delay 10 xc:lime -delay 15 xc:blue -loop 0 APNG:anim.png
cjxl anim.gif anim.jxl >/dev/null 2>&1

# Three pages.
magick -size 16x12 xc:red xc:lime xc:blue pages.tif

cat > drawing.svg <<'EOF'
<svg xmlns="http://www.w3.org/2000/svg" width="16" height="12">
  <rect width="8" height="6" fill="red"/>
  <rect x="8" width="8" height="6" fill="lime"/>
  <rect y="6" width="8" height="6" fill="blue"/>
  <rect x="8" y="6" width="8" height="6" fill="white"/>
</svg>
EOF
gzip -9 -n -c drawing.svg > drawing.svgz
