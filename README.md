<h1 align=center><code>para</code></h1>

<div align=center>
  <a href=https://crates.io/crates/para>
    <img src=https://img.shields.io/crates/v/para.svg alt="crates.io version">
  </a>
  <a href=https://github.com/parasitepool/para/actions/workflows/ci.yaml>
    <img src=https://github.com/parasitepool/para/actions/workflows/ci.yaml/badge.svg alt="build status">
  </a>
  <a href=https://github.com/parasitepool/para/releases>
    <img src=https://img.shields.io/github/downloads/parasitepool/para/total.svg alt=downloads>
  </a>
</div>
<br>

## Hermit Environment

A full development/build environment is bundled using hermit and can be activated as follows:
```
. ./bin/activate-hermit
```

## Building the docs

```
cargo install mdbook mdbook-linkcheck
just build-docs
just serve-docs
```

Then you can customize CSS and javascript by following [this
guide](https://github.com/rust-lang/mdBook/tree/master/guide/src/format/theme)
and doing:

```
just init-mdbook-theme
```

This will create the default `mdbook` layout and CSS files inside
`docs/tmp/theme`, which you can then pick, chose and adapt and then copy into
`docs/theme` to tweak the defaults.
