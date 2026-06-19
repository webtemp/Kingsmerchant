# Publishing kingsmerchant to the AUR

The AUR hosts only the `PKGBUILD`, `.SRCINFO`, and `kingsmerchant.install` — the
source is fetched from a **GitHub release tag** at install time. So GitHub comes
first, the AUR second.

## 0. Repo prerequisites (do these once, before tagging)

- [ ] **License files exist in the repo root** (`LICENSE-MIT` and
      `LICENSE-GPL`, both present) — `package()` installs both, and `namcap`
      flags MIT otherwise.
- [ ] **`Cargo.lock` is committed** (it is) — required for the offline
      `--frozen` build.

## 1. Publish the source (GitHub)

```sh
# in the kingsmerchant repo
git tag v0.9.1
git push origin v0.9.1          # or cut a GitHub Release for tag v0.9.1
```

## 2. Finalize the PKGBUILD

`_gh` is already set to the project's GitHub account (`webtemp`); change it only
if you fork.

```sh
cd packaging/arch
updpkgsums                       # replaces sha256sums=('SKIP') with the real digest
makepkg --printsrcinfo > .SRCINFO   # regenerate so it matches the PKGBUILD exactly
```

## 3. Verify it builds on a clean system (catches missing deps)

```sh
namcap PKGBUILD                  # lint the recipe
makepkg -f                       # quick local build
# Best: a pristine chroot — your own box has everything, a user's won't:
#   pkgctl build          (devtools)   OR   extra-x86_64-build
namcap kingsmerchant-*.pkg.tar.zst     # lint the built package
```

## 4. AUR account (one-time)

- Register at https://aur.archlinux.org and add your **SSH public key** under
  *My Account* (the AUR is SSH-git only).

## 5. Push to the AUR

```sh
git clone ssh://aur@aur.archlinux.org/kingsmerchant.git aur-kingsmerchant
cd aur-kingsmerchant
cp ../kingsmerchant/packaging/arch/{PKGBUILD,.SRCINFO,kingsmerchant.install} .
git add PKGBUILD .SRCINFO kingsmerchant.install
git commit -m "Initial import: kingsmerchant 0.9.1"
git push
```

Only ever commit `PKGBUILD`, `.SRCINFO`, and `kingsmerchant.install` — never built
packages or `src/`/`pkg/` dirs.

## Updating later

Bump `pkgver` (and reset `pkgrel=1`) → tag a new GitHub release → `updpkgsums`
→ regenerate `.SRCINFO` → commit & push to the AUR.

## Notes

- CachyOS / EndeavourOS / Garuda users get this for free — they all use the AUR.
- The `kingsmerchant.install` scriptlet prints the **`input` group** reminder on
  install; pacman cannot add the user to a group itself.
