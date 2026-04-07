# 用户配置管理

## POST /api/user/sw_token
- 注入seewoToken。存储到一个全局变量，可供各个模块获取，可以覆盖更新。
### 请求体：
```
{
  "token": "xxxxx-xx"
}
```
### 响应：
```
{}
```

## GET /api/user/call_name
- 获取个性化称呼信息。获取到设定的用户称呼和claw的称呼。（是要在SOUL、IDENTITY、USER文件里面获取？）
### 响应：
```
{
  "user_name": "teacher",
  "claw_name": "小希助手"
}
```

## POST /api/user/call_name
- 设置个性化称呼信息。设置设定的用户称呼和claw的称呼。（是要在SOUL、IDENTITY、USER文件里面设置？）
### 请求体：
```
{
  "user_name": "teacher",
  "claw_name": "小希助手"
}
```
### 响应：
```
{}
```