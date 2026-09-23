use collocate_core::{Error, Result};
use collocate_sys::mount::FsContext;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsOpt {
    Str(String, String),
    Flag(String),
}

impl FsOpt {
    pub fn key(&self) -> &str {
        match self {
            FsOpt::Str(k, _) | FsOpt::Flag(k) => k,
        }
    }

    pub fn value(&self) -> Option<&str> {
        match self {
            FsOpt::Str(_, v) => Some(v),
            FsOpt::Flag(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayPlan {
    pub lowers: Vec<PathBuf>,
    pub upper: PathBuf,
    pub work: PathBuf,
    pub volatile: bool,
}

fn text(p: &std::path::Path) -> Result<String> {
    p.to_str().map(String::from).ok_or_else(|| Error::Invalid(format!("non-utf8 path {p:?}")))
}

impl OverlayPlan {
    pub fn options(&self) -> Result<Vec<FsOpt>> {
        if self.lowers.is_empty() {
            return Err(Error::InvalidSpec("overlay needs at least one lower layer".into()));
        }
        let mut out = Vec::new();
        for l in &self.lowers {
            out.push(FsOpt::Str("lowerdir+".into(), text(l)?));
        }
        out.push(FsOpt::Str("upperdir".into(), text(&self.upper)?));
        out.push(FsOpt::Str("workdir".into(), text(&self.work)?));
        if self.volatile {
            out.push(FsOpt::Flag("volatile".into()));
        }
        Ok(out)
    }

    pub fn mount(&self, target: &str) -> Result<()> {
        let ctx = FsContext::open("overlay")?;
        for opt in self.options()? {
            match opt {
                FsOpt::Str(k, v) => ctx.set_string(&k, &v)?,
                FsOpt::Flag(k) => ctx.set_flag(&k)?,
            }
        }
        ctx.create()?;
        ctx.mount_at(target, 0)?;
        Ok(())
    }
}
