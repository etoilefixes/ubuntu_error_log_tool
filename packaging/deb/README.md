# Debian Packaging Notes

- `logtool.service`: service file used in the `.deb` package.
  - Installed to `/usr/lib/systemd/system/logtool.service`.
  - Uses `/usr/bin/logtool-daemon` as `ExecStart`.
- `postinst`: creates `logtool` group (if missing), reloads systemd, enables and starts service.
- `prerm`: stops and disables service on remove/deconfigure.
- `postrm`: reloads systemd after removal.

Build package from project root:

```bash
./scripts/build-deb.sh
```
