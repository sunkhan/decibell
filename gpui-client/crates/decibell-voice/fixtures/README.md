The stream posters the voice screens and their Electron twins show (`src/fixture.rs`): 640 × 360
JPEG, made once with ImageMagick 7 (the bytes are committed, so both sides draw the same picture):

    magick -size 640x360 gradient:'#1e3a5f-#0b1320' -fill '#2f6f9f' -draw 'rectangle 40,40 380,220' \
        -fill '#e8b04a' -draw 'circle 500,250 560,250' -fill '#d0d8e0' -draw 'rectangle 40,250 300,270' \
        -draw 'rectangle 40,285 240,300' -quality 80 -strip poster-carol.jpg
    magick -size 640x360 gradient:'#3b1f4a-#120a18' -fill '#7a3f9a' -draw 'polygon 80,300 320,60 560,300' \
        -fill '#5ad19a' -draw 'rectangle 60,40 200,90' -quality 80 -strip poster-bob.jpg
