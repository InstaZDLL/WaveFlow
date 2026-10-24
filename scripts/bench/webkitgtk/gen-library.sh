#!/usr/bin/env bash
# Generate the bench's fixture library: artists / albums / tracks with
# folder covers, artist photos, synced lyrics and a beat at a known tempo,
# so the bench never needs anyone's real music and every run sees the
# same catalogue.
#
#   bash gen-library.sh <dest-dir> [albums=240]
#
# Layout: <dest>/Artist NN/Album NNN/{cover.jpg, 01 ….flac, 02 ….flac,
# 01 ….lrc}, plus <dest>/Artist NN/artist.jpg. Tracks are 120 s mono FLAC
# at 22.05 kHz: a 220 Hz tone struck on each beat (70–160 BPM by album),
# small on disk and analysable for Mood Radio. Covers alternate between
# pale, dark and saturated gradients, the three cases the cover-lit
# backgrounds have to survive. Needs ffmpeg with the `gradients` and
# `aevalsrc` filters (8.x).
set -euo pipefail
DEST="${1:?destination directory}"
ALBUMS="${2:-240}"
ARTISTS=$(( (ALBUMS + 3) / 4 ))
mkdir -p "$DEST"
GENRES=(Pop Rock Electronic Jazz Hip-Hop Classical Ambient Folk)

cover() { # <out.jpg> <seed> <kind>
  local out="$1" seed="$2" kind="$3" eq
  case "$kind" in
    pale) eq="eq=brightness=0.35:saturation=0.45" ;;
    dark) eq="eq=brightness=-0.35:saturation=0.9" ;;
    *) eq="eq=saturation=1.4" ;;
  esac
  ffmpeg -loglevel error -y -f lavfi \
    -i "gradients=s=800x800:seed=${seed}:n=4:speed=0:type=radial" \
    -frames:v 1 -vf "$eq" -q:v 4 "$out"
}

lrc() { # <out.lrc> <title>
  local out="$1" title="$2" s
  : > "$out"
  for s in $(seq 0 3 117); do
    printf '[%02d:%02d.00]%s, line %d of the fixture\n' $((s / 60)) $((s % 60)) "$title" $((s / 3 + 1)) >> "$out"
  done
}

for a in $(seq 1 "$ARTISTS"); do
  artist=$(printf 'Artist %02d' "$a")
  mkdir -p "$DEST/$artist"
  [ -f "$DEST/$artist/artist.jpg" ] || cover "$DEST/$artist/artist.jpg" $((a * 7919)) vivid
done

for n in $(seq 1 "$ALBUMS"); do
  a=$(( (n - 1) / 4 + 1 ))
  artist=$(printf 'Artist %02d' "$a")
  album=$(printf 'Album %03d' "$n")
  dir="$DEST/$artist/$album"
  [ -f "$dir/02 Second $n.flac" ] && continue
  mkdir -p "$dir"
  case $((n % 3)) in 0) kind=pale ;; 1) kind=dark ;; *) kind=vivid ;; esac
  cover "$dir/cover.jpg" "$n" "$kind"
  bpm=$(( 70 + (n * 37) % 91 ))
  genre=${GENRES[$((n % ${#GENRES[@]}))]}
  year=$(( 1990 + n % 35 ))
  for t in 1 2; do
    if [ "$t" = 1 ]; then title="First $n"; else title="Second $n"; fi
    file="$dir/0$t $title.flac"
    ffmpeg -loglevel error -y -f lavfi \
      -i "aevalsrc='0.4*sin(2*PI*220*t)*exp(-18*mod(t,60/${bpm}))':s=22050:d=120" \
      -ac 1 -sample_fmt s16 \
      -metadata title="$title" -metadata artist="$artist" \
      -metadata album_artist="$artist" -metadata album="$album" \
      -metadata date="$year" -metadata genre="$genre" \
      -metadata track="$t/2" "$file"
  done
  lrc "$dir/01 First $n.lrc" "First $n"
done
echo "library: $ALBUMS albums, $((ALBUMS * 2)) tracks, $ARTISTS artists in $DEST"
