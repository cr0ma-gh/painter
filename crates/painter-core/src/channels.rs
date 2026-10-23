//! Colour channels edited apart (Photoshop's Channels panel): an edit that
//! should only touch some of red, green and blue gets the others back from
//! the state before it.

use crate::blend::{premultiply, unpremultiply};
use crate::tile::{TileCoord, TileStore};

/// In the `coords` tiles of `tiles`, the colour channels not in `edit`
/// (`[red, green, blue]`) go back to what they were in `before`; alpha and
/// the edited channels keep the change.
pub fn keep_channels(tiles: &mut TileStore, before: &TileStore, coords: impl IntoIterator<Item = TileCoord>, edit: [bool; 3]) {
    if edit == [true; 3] {
        return;
    }
    for coord in coords {
        let Some(after) = tiles.get(coord).map(|t| (**t).clone()) else { continue };
        let old = before.get(coord);
        let mut tile = after;
        for (i, px) in tile.bytes_mut().as_chunks_mut::<4>().0.iter_mut().enumerate() {
            if px[3] == 0 {
                continue;
            }
            let was = old.map_or([0; 4], |t| unpremultiply(t.bytes().as_chunks::<4>().0[i]));
            let mut now = unpremultiply(*px);
            for c in 0..3 {
                if !edit[c] {
                    now[c] = was[c];
                }
            }
            *px = premultiply(now);
        }
        tiles.insert(coord, tile);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_edited_channels_change() {
        let grey = TileStore::from_rgba(64, 64, &[100, 100, 100, 255].repeat(64 * 64));
        let mut painted = TileStore::from_rgba(64, 64, &[250, 10, 30, 255].repeat(64 * 64));
        keep_channels(&mut painted, &grey, [TileCoord::new(0, 0)], [true, false, false]);
        assert_eq!(painted.pixel(3, 3), [250, 100, 100, 255]);
    }
}
