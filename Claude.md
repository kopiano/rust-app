# Claude编程规范

* 代码尽量放在一行内
* 路由和方法放在一行，如.route("/message/image", post(message::send_image).layer(DefaultBodyLimit::max(12 * 1024 * 1024)))
* 不要移除我写的注释
* 不要随便将我代码一行改为多行
* 表名不要加s后缀, 路由也不要加s后缀
* 多线程共享数据，请用 Arc；需要内部修改，请套 Arc<Mutex<T>>
* handles中的.bind等方法还是每个放一行比较美观