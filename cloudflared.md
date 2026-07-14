# cloudflare tunnel


```shell
$ cloudflared tunnel create rust-app
$ ls -la ~/.cloudflared
$ vim ~/.cloudflared/rust-app.yml
#tunnel: 8dddca8b-8b7c-4909-bd2a-d1df0ac46506
#credentials-file: /Users/coulsonzero/.cloudflared/8dddca8b-8b7c-4909-bd2a-d1df0ac46506.json
#
#ingress:
# - hostname: app.coulsonzero.shop
#   service: http://localhost:8100
#
# - service: http_status:404
$ cloudflared tunnel route dns rust-app app.coulsonzero.shop
$ cargo run
$ cloudflared tunnel --config ~/.cloudflared/rust-app.yml --protocol http2 run rust-app
```



* 查看tunnel
```shell
cloudflared tunnel list
cloudflared tunnel delete alpha-backend
cloudflared tunnel ingress validate
```

* WRN Your `version` 2026.6.1 is outdated. We recommend upgrading it to 2026.7.1
```sh
brew upgrade cloudflared
```

```sh
cloudflared tunnel ingress validate
```
