# AUR `waveflow-bin`

A `-bin` package that downloads the official `.deb` from GitHub
Releases and unpacks it into the Arch filesystem layout. Updates land
on AUR automatically through `.github/workflows/aur.yml` after every
new GitHub release.

## First-time setup (one-off, by hand)

1. Create an [AUR account](https://aur.archlinux.org/register/) for
   the maintainer.
2. Generate an SSH key dedicated to AUR pushes and add the public
   half to your AUR profile (Account → SSH Public Key):
   ```sh
   ssh-keygen -t ed25519 -f ~/.ssh/aur -C 'aur:waveflow'
   cat ~/.ssh/aur.pub
   ```
3. Reserve the package name:
   ```sh
   git clone ssh://aur@aur.archlinux.org/waveflow-bin.git ~/aur/waveflow-bin
   ```
   (the AUR creates the empty repo on first connect when no package
   with that name exists)
4. Drop the PKGBUILD in, generate `.SRCINFO`, and push:
   ```sh
   cp packaging/aur/PKGBUILD ~/aur/waveflow-bin/
   cd ~/aur/waveflow-bin
   updpkgsums
   makepkg --printsrcinfo > .SRCINFO
   git add PKGBUILD .SRCINFO
   git commit -m 'Initial release: 0.1.0-1'
   git push -u origin master
   ```
5. In the WaveFlow GitHub repo, add the AUR private key as the
   `AUR_SSH_PRIVATE_KEY` secret (Settings → Secrets → Actions). The
   release workflow uses it to push subsequent updates.

## Per-release process

Once the secret is in place, no manual AUR work is needed: the
`aur` workflow takes over after every successful GitHub release.

To smoke-test the build locally:

```sh
cd packaging/aur
cp PKGBUILD /tmp/waveflow-aur && cd /tmp/waveflow-aur
updpkgsums
makepkg -si
```
