# Publishing poe2ddd to the AUR

The AUR hosts only the `PKGBUILD`, `.SRCINFO`, and `poe2ddd.install` — the
source is fetched from a **GitHub release tag** at install time. So GitHub comes
first, the AUR second.

## 0. Repo prerequisites (do these once, before tagging)

- [ ] **Add license files to the repo root.** `Cargo.toml` declares
      `MIT OR GPL-3.0-or-later`, but no text ships. Create `LICENSE-MIT` and
      `LICENSE-GPL3` — `package()` installs both, and `namcap` flags MIT
      otherwise.
- [ ] **`Cargo.lock` is committed** (it is) — required for the offline
      `--frozen` build.

## 1. Publish the source (GitHub)

```sh
# in the poe2ddd repo
git tag v0.1.0
git push origin v0.1.0          # or cut a GitHub Release for tag v0.1.0
```

## 2. Finalize the PKGBUILD

```sh
cd packaging/arch
sed -i 's/^_gh=OWNER/_gh=YOUR_GITHUB_USERNAME/' PKGBUILD

updpkgsums                       # replaces sha256sums=('SKIP') with the real digest
makepkg --printsrcinfo > .SRCINFO   # regenerate so it matches the PKGBUILD exactly
```

## 3. Verify it builds on a clean system (catches missing deps)

```sh
namcap PKGBUILD                  # lint the recipe
makepkg -f                       # quick local build
# Best: a pristine chroot — your own box has everything, a user's won't:
#   pkgctl build          (devtools)   OR   extra-x86_64-build
namcap poe2ddd-*.pkg.tar.zst     # lint the built package
```

## 4. AUR account (one-time)

- Register at https://aur.archlinux.org and add your **SSH public key** under
  *My Account* (the AUR is SSH-git only).

## 5. Push to the AUR

```sh
git clone ssh://aur@aur.archlinux.org/poe2ddd.git aur-poe2ddd
cd aur-poe2ddd
cp ../poe2ddd/packaging/arch/{PKGBUILD,.SRCINFO,poe2ddd.install} .
git add PKGBUILD .SRCINFO poe2ddd.install
git commit -m "Initial import: poe2ddd 0.1.0"
git push
```

Only ever commit `PKGBUILD`, `.SRCINFO`, and `poe2ddd.install` — never built
packages or `src/`/`pkg/` dirs.

## Updating later

Bump `pkgver` (and reset `pkgrel=1`) → tag a new GitHub release → `updpkgsums`
→ regenerate `.SRCINFO` → commit & push to the AUR.

## Notes

- CachyOS / EndeavourOS / Garuda users get this for free — they all use the AUR.
- The `poe2ddd.install` scriptlet prints the **`input` group** reminder on
  install; pacman cannot add the user to a group itself.
