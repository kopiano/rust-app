# API 路由

### auth
┌────────┬─────────────────┬──────────────┐                                                                                                                              
│  方法   │      路径       │     功能      │                                                                                                                              
├────────┼─────────────────┼──────────────┤                                                                                                                              
│ POST   │  /api/register  │     注册      │                                                                                                                              
├────────┼─────────────────┼──────────────┤                                                                                                                               
│ POST   │  /api/login     │     登录      │                                                                                                                              
└────────┴─────────────────┴──────────────┘   
**register**
注册 POST /api/auth/register
┌──────────┬────────┬──────┐                                                                                                                                             
│   字段    │  类型  │  必填 │                                                                                                                                             
├──────────┼────────┼──────┤                                                                                                                                             
│ name     │ string │  是  │                                                                                                                                             
├──────────┼────────┼──────┤                                                                                                                                             
│ email    │ string │  是  │                                                                                                                                             
├──────────┼────────┼──────┤                                                                                                                                             
│ password │ string │  是  │                                                                                                                                             
└──────────┴────────┴──────┘
请求体：
```json
{                                                                                                                                                                        
  "name": "张三",                                                                                                                                                      
  "email": "zhangsan@example.com",                                                                                                                                     
  "password": "123456"                                                                                                                                                 
}
```
201 Created 响应： 
```json
{                                                                                                                                                                        
    "token": "eyJhbGciOiJIUzI1NiJ9...",                                                                                                                                  
    "user": {                                                                                                                                                            
        "id": "f47ac10b-58cc-4372-a567-0e02b2c3d479",                                                                                                                    
        "name": "张三",                                                                                                                                                  
        "email": "zhangsan@example.com",                                                                                                                                 
        "created_at": "2026-07-11T12:00:00Z",                                                                                                                            
        "updated_at": "2026-07-11T12:00:00Z"                                                                                                                             
    }                                                                                                                                                                    
}
```

- 注册重复用户名或邮箱返回明确的 409
- 注册数据库异常记录日志并返回 500
- 校验 name、email、password
- 密码使用 bcrypt 加密
- 写入 user 表
- 返回 JWT 和用户信息
- 设置登录 Cookie
- 注册成功后写入：
    - last_login_at = NOW()

**login**
登录 POST /api/auth/login
┌──────────┬────────┬──────┐                                                                                                                                             
│   字段    │  类型  │ 必填  │                                                                                                                                             
├──────────┼────────┼──────┤                                                                                                                                             
│ email    │ string │  是  │                                                                                                                                             
├──────────┼────────┼──────┤                                                                                                                                             
│ password │ string │  是  │                                                                                                                                             
└──────────┴────────┴──────┘
请求体：             
```json
{                                                                                                                                                                        
  "email": "zhangsan@example.com",                                                                                                                                     
  "password": "123456"                                                                                                                                                 
}
```
200 OK 响应：
```json
{                                                                                                                                                                        
    "token": "eyJhbGciOiJIUzI1NiJ9...",                                                                                                                                  
    "user": {                                                                                                                                                            
        "id": "f47ac10b-58cc-4372-a567-0e02b2c3d479",                                                                                                                    
        "name": "张三",                                                                                                                                                  
        "email": "zhangsan@example.com",                                                                                                                                 
        "created_at": "2026-07-11T12:00:00Z",                                                                                                                            
        "updated_at": "2026-07-11T12:00:00Z"                                                                                                                             
    }                                                                                                                                                                    
}
```
401 Unauthorized — 邮箱或密码错误
登录bcrypt校验耗时很高，const BCRYPT_COST: u32 = 8;极大降低耗时
| Cost |       大致耗时 |
| ---- | ---------: |
| 8    |   20–40 ms |
| 10   |  50–120 ms |
| 12   | 150–400 ms |
| 14   | 500 ms–1 s |
cost12-8: 900ms->100ms

**me**
GET /api/auth/me          
需要 JWT 鉴权
Authorization: Bearer <token>
返回：  
```json
{                                                                                                                                                                        
  "id": "f47ac10b-58cc-4372-a567-0e02b2c3d479",                                                                                                                        
  "name": "张三",                                                                                                                                                      
  "email": "zhangsan@example.com",                                                                                                                                     
  "created_at": "2026-07-11T12:00:00Z",                                                                                                                                
  "updated_at": "2026-07-11T12:00:00Z"                                                                                                                                 
}
```


### user
┌────────┬─────────────────┬──────────────┐                                                                                                                              
│  方法   │      路径       │     功能      │                                                                                                                              
├────────┼─────────────────┼──────────────┤                                                                                                                              
│ GET    │ /               │ Hello World  │                                                                                                                              
├────────┼─────────────────┼──────────────┤                                                                                                                              
│ GET    │ /api/user       │ 列表所有用户   │                                                                                                                              
├────────┼─────────────────┼──────────────┤                                                                                                                              
│ POST   │ /api/user       │ 创建用户      │                                                                                                                              
├────────┼─────────────────┼──────────────┤                                                                                                                              
│ GET    │ /api/user/{id}  │ 获取单个用户   │                                                                                                                               
├────────┼─────────────────┼──────────────┤                                                                                                                              
│ PUT    │ /api/user/{id}  │ 更新用户       │                                                                                                                              
├────────┼─────────────────┼──────────────┤                                                                                                                              
│ DELETE │ /api/user/{id}  │ 删除用户       │                                                                                                                              
└────────┴─────────────────┴──────────────┘    

register：public，不需要jwt鉴权，面向外部普通公众开放
post /user：private，面向内部管理员，用于管理后台账号

### task
路由 — 全部受 JWT 保护：

┌────────┬─────────────────┬────────────────────────┐                                                                                                                    
│  方法   │      路径       │          功能           │                                                                                                                    
├────────┼─────────────────┼────────────────────────┤                                                                                                                    
│ GET    │ /api/task       │ 当前用户的 task 列表     │                                                                                                                    
├────────┼─────────────────┼────────────────────────┤                                                                                                                    
│ POST   │ /api/task       │ 创建 task               │                                                                                                                    
├────────┼─────────────────┼────────────────────────┤                                                                                                                    
│ GET    │ /api/task/{id}  │ 获取单个 task            │                                                                                                                    
├────────┼─────────────────┼────────────────────────┤                                                                                                                    
│ PUT    │ /api/task/{id}  │ 更新 title / completed  │                                                                                                                    
├────────┼─────────────────┼────────────────────────┤                                                                                                                    
│ DELETE │ /api/task/{id}  │ 删除 task               │                                                                                                                    
└────────┴─────────────────┴────────────────────────┘

所有操作限制了 user_id = claims.sub，用户只能操作自己的 task``

## chat

### 获取联系人列表信息(群聊和用户)
GET /api/message/user_info
Authorization: Bearer <token>
```json
{
    "code": 200,
    "message": "success",
    "data": [
      {
        "user_id": "2c7f0f4d-9a1c-4e8b-a9d0-1f3b4e5c6a77",
        "group_id": null,
        "chat_type": "private",
        "avatar": "https://cdn.example.com/avatar/alice.png",
        "username": "Alice",
        "online": true,
        "content": "晚上一起测试接口吗？",
        "last_message_time": "2026-07-14T21:20:11Z",
        "members": []
      },
      {
        "user_id": null,
        "group_id": "8f2b8d6e-2f91-4bd4-bc3b-8c1a4f2f93aa",
        "chat_type": "public",
        "avatar": "https://cdn.example.com/group/rust.png",
        "username": "Rust 开发群",
        "content": "明天发布新版本",
        "last_message_time": "2026-07-14T21:18:42Z",
        "members": [
          {
            "user_id": "2c7f0f4d-9a1c-4e8b-a9d0-1f3b4e5c6a77",
            "avatar": "https://cdn.example.com/avatar/alice.png",
            "username": "Alice",
            "online": true
          },
          {
            "user_id": "9d4a6b21-6b1f-48e5-9bca-7f9c8a1d2e33",
            "avatar": "https://cdn.example.com/avatar/bob.png",
            "username": "Bob",
            "online": false
          }
        ]
      }
    ]
  }
```

## user
### profile 用户信息修改
PUT /api/user/profile
Authorization: Bearer <access_token>
Content-Type: application/json
请求参数：
```
  {
    "username": "new_username",
    "avatar": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAA...",
    "password": "NewPassword123"
  }
```
字段说明：
- username：新用户名，必填
- avatar：Base64 图片，必填，服务端会转换为 WebP
- password：新密码；传空字符串表示不修改密码
  成功返回：
```json
  {
    "code": 200,
    "message": "success",
    "data": {
      "id": "2c7f0f4d-9a1c-4e8b-a9d0-1f3b4e5c6a77",
      "name": "new_username",
      "email": "user@example.com",
      "github_id": null,
      "avatar": "/api/assets/avatar/new-username-1752585600000.webp",
      "last_login_at": "2026-07-15T10:20:30Z",
      "created_at": "2026-07-01T08:00:00Z",
      "updated_at": "2026-07-15T10:25:00Z"
    }
  }

  
```
注意：密码不会出现在返回结果中。GitHub 用户当前不允许通过此接口修改资料，会返回 403。

### moment

POST /moment
```json
{
    "content":"Hello",
    "media":[
        {
            "type":"image",
            "url":"..."
        }
    ]
}
```
GET /moment

POST /moment/:id/like        点赞
DELETE /moment/:id/unlike    取消点赞
POST /moment/:id/comment     评论
GET /moment/:id/comment      获取评论


## video
### 视频浏览

接口                             作用                                                                  登录                                                                
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━  ━━━━━━
GET /api/video                  获取视频列表，支持搜索、分类、收藏、个人视频、集合筛选和游标分页                 可选
──────────────────────────────  ──────────────────────────────────────────────────────────────────  ──────
GET /api/video/{id}             获取视频详情、封面、HLS 播放地址和互动数据                                  可选
──────────────────────────────  ──────────────────────────────────────────────────────────────────  ──────
GET /api/video/categories       获取视频分类列表                                                        可选
──────────────────────────────  ──────────────────────────────────────────────────────────────────  ──────
GET /api/video/collections      获取视频集合列表及集合封面                                                可选
──────────────────────────────  ──────────────────────────────────────────────────────────────────  ──────
GET /api/video/{id}/comments    获取视频评论                                                            可选
──────────────────────────────  ──────────────────────────────────────────────────────────────────  ──────
POST /api/video/{id}/view       记录视频播放量                                                          可选
──────────────────────────────  ──────────────────────────────────────────────────────────────────  ──────
### 视频播放

详情接口返回：

{
"hls_master_url": "/api/assets/video/user/video-id/master.m3u8",
"cover_url": "/api/assets/video/user/video-id/cover.webp"
}
### 视频互动

接口                                    作用                                                                                                                            
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━  ━━━━━━━━━━━━━━
POST /api/video/{id}/like               点赞
──────────────────────────────────────  ──────────────
DELETE /api/video/{id}/like             取消点赞
──────────────────────────────────────  ──────────────
POST /api/video/{id}/favorite           收藏
──────────────────────────────────────  ──────────────
DELETE /api/video/{id}/favorite         取消收藏
──────────────────────────────────────  ──────────────
POST /api/video/{id}/comments           发布评论
──────────────────────────────────────  ──────────────
POST /api/video/comments/{id}/like      点赞评论
──────────────────────────────────────  ──────────────
DELETE /api/video/comments/{id}/like    取消评论点赞
──────────────────────────────────────  ──────────────
### 视频管理
接口                                  作用                                                                                                                              
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
PATCH /api/video/{id}                 修改标题、描述、可见性、分类等
────────────────────────────────────  ────────────────────────────────
DELETE /api/video/{id}                删除视频
────────────────────────────────────  ────────────────────────────────
POST /api/video/collections           创建视频集合
────────────────────────────────────  ────────────────────────────────
PATCH /api/video/collections/{id}     修改视频集合
────────────────────────────────────  ────────────────────────────────
DELETE /api/video/collections/{id}    删除视频集合


### 获取视频列表封面信息（进入视频页面获取）
GET /api/video
常用参数：
GET /api/video?scope=public&limit=20
注意：不用返回视频url

响应示例：
```json
{
  "code": 200,
  "message": "success",
  "data": {
    "items": [
      {
        "id": "video-id",
        "title": "刺绣视频",
        "cover_url": "/api/assets/video/user/video-id/cover.webp",
        "status": "ready"
      }
    ],
    "has_more": true,
    "next_before_created_at": "2026-08-03T09:00:00Z",
    "next_before_id": "video-id"
  }
}
```

### 视频详情接口(点击视频封面获取)
GET /api/videos/{video_id}
例如：
GET /api/video/5ec5d7bd-196a-43cf-b58f-d17e976bade5

## 商城订单（接口设计）

订单接口均要求登录，并通过 `Authorization: Bearer <token>` 确认当前用户。用户只能读取自己的购物车和订单。金额以整数分返回，创建订单时由服务端读取商品价格并计算，不信任客户端提交的价格或总价。

### 提交购物车为待处理订单

`POST /api/store/orders`

请求体：
```json
{
  "idempotency_key": "8bc9ed8b-bf7a-40d4-a524-485f50e7c98a"
}
```

`idempotency_key` 是客户端为一次下单操作生成的 UUID。网络超时或响应丢失后，重试必须使用同一个键；同一用户使用相同键重复提交时，服务端返回已创建的订单，不再创建第二笔订单。用户开始一次新的下单操作时应生成新键。

首次创建成功返回 `201 Created`，重复请求返回 `200 OK`：
```json
{
  "code": 200,
  "message": "success",
  "data": {
    "id": "0e71c6f8-65f5-4548-8bd5-20f42bc8858e",
    "status": "pending",
    "total": 1390,
    "created_at": "2026-10-26T10:00:00Z",
    "updated_at": "2026-10-26T10:00:00Z"
  }
}
```

空购物车返回 `400 Bad Request`。服务端在数据库事务中读取并冻结购物车商品、规格、数量和价格快照，创建订单及订单明细，写入待发布事件，并清空已提交的购物车。事务提交后由 outbox 发布器将 `store.order.created` 事件投递到 RabbitMQ；队列暂不可用时，事件留在 outbox 中等待重试。

### 查询当前用户订单

`GET /api/store/orders`

返回当前用户最近创建的订单列表，最多 50 条。列表项包含订单 ID、状态、总金额（分）、创建及更新时间。

### 查询订单详情

`GET /api/store/orders/{id}`

仅订单所属用户可以读取。响应中的 `data` 包含订单字段及 `items` 明细；每项包含商品 ID、下单时的商品名称、规格、温度、甜度、数量、单价（分）和行金额（分）。无权访问或订单不存在时返回 `404 Not Found`。

### 状态流转与消息

订单状态：

| 状态 | 含义 |
| --- | --- |
| `pending` | 订单已落库，等待 RabbitMQ 消费者处理 |
| `processing` | 消费者已接收并开始处理 |
| `confirmed` | 此流程的异步处理已完成 |
| `failed` | 异步处理最终失败（保留为后续失败处理状态） |

RabbitMQ 事件只携带订单 ID 等必要标识，不包含支付凭据。消费者按订单 ID 幂等推进状态；重复投递不得重复创建订单或重复执行已完成的状态转换。消费者处理失败时消息重新入队，交由后续重试。

### 不扣款边界

以上流程只负责创建订单、排队并推进订单状态，不调用支付渠道，不创建支付单，不扣款，不确认到账，也不发放与支付成功绑定的权益。`confirmed` 仅表示本订单流程的异步处理已完成，不代表已付款。未来如需支付，应另行设计独立的支付接口、支付状态和回调校验流程。
