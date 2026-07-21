# cloudflare tunnel
step1: cloudflare 官网
- 添加域名: kopaino.cc
- Aliyun域名列表-DNS管理-DNS修改-添加两条NS：cass.ns.cloudflare.com / neil.ns.cloudflare.com
- 以后阿里云之负责注册和实名认证，Cloudflare负责DNS记录，免费自带HTTPS等安全防护

step2: 关闭vpn

step3: 配置tunnel
```sh
$ cloudflared tunnel create backend-api
$ vim ~/.cloudflared/config.yml 
$ cloudflared tunnel route dns backend-api a.kopiano.cc   
$ cloudflared tunnel route dns backend-api b.kopiano.cc 
$ cloudflared tunnel run backend-api
```
config.yml
```yml
tunnel: 89f4a205-27a6-4fd0-b927-36986eca9791
credentials-file: /Users/coulsonzero/.cloudflared/89f4a205-27a6-4fd0-b927-36986eca9791.json
protocol: http2

ingress:
  - hostname: a.kopiano.cc              # rust-app
    service: http://localhost:8100
  - hostname: b.kopiano.cc              # go-alpha
    service: http://localhost:8000
  - service: http_status:404                
```

其它命令
```shell
$ ls -la ~/.cloudflared
$ cloudflared tunnel list
$ cloudflared tunnel delete alpha-backend
$ cloudflared tunnel ingress validate
$ cloudflared tunnel --config ~/.cloudflared/rust-app.yml --protocol http2 run rust-app
$ cloudflared tunnel login
$ vim ~/.cloudflared/cert.pem
```

* WRN Your `version` 2026.6.1 is outdated. We recommend upgrading it to 2026.7.1
```sh
brew upgrade cloudflared
```


当前只是你的 macOS 本机 DNS 仍有旧缓存。执行：
```sh
sudo dscacheutil -flushcache
sudo killall -HUP mDNSResponder
```
然后关闭并重新打开终端、浏览器，再测试：
curl https://a.kopiano.cc/api/health
正常应返回：
{"status":"ok"}
