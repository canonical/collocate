use crate::config::{ImageMeta, ImageStore};
use collocate_core::spec::ImageKind;

fn runs_pebble(argv: &[String]) -> bool {
    argv.first().is_some_and(|a| a.rsplit('/').next() == Some("pebble"))
}

pub fn detect(store: &ImageStore, meta: &ImageMeta) -> ImageKind {
    if runs_pebble(&meta.config.entrypoint) || (meta.config.entrypoint.is_empty() && runs_pebble(&meta.config.cmd)) {
        return ImageKind::Pebble;
    }
    let shipped = meta.layers.iter().rev().any(|l| {
        let dir = store.layer_dir(l);
        ["bin/pebble", "usr/bin/pebble"].iter().any(|p| dir.join(p).symlink_metadata().is_ok())
    });
    if shipped {
        ImageKind::Pebble
    } else {
        ImageKind::Oci
    }
}
