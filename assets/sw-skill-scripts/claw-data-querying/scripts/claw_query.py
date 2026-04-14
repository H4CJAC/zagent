#!/usr/bin/env python3
"""
Claw 数据查询工具

供外部 Claw 调用，通过 scope WebSocket 执行个人数据查询。

用法:
    # 使用 JWT token
    python claw_query.py --token "xxx" --question "查询我的订单"

    # 使用 UC token（需指定 --uc 和 --app-id/--app-key/--app-secret）
    python claw_query.py --token "xxx" --question "查询我的订单" --uc --app-key "your_app_key"

环境变量:
    CLAW_WS_URL: WebSocket 地址 (默认: ws://chatdata-agent.test.seewo.com/api/ws)
    CLAW_TENANT_ID: 租户ID (默认: chatdata-mofang-school)
    CLAW_APP_KEY: App Key (默认: supersonic)
"""
from __future__ import annotations

import asyncio
import json
import os
import sys
import argparse

try:
    import websockets
except ImportError:
    print("请安装 websockets: pip install websockets")
    sys.exit(1)


WS_URL = os.getenv("CLAW_WS_URL", "ws://chatdata-agent.seewo.com/api/ws")
TENANT_ID = os.getenv("CLAW_TENANT_ID", "chatdata-mofang-school")
APP_KEY = os.getenv("CLAW_APP_KEY", "EasiNote5")  # UC 模式固定使用 EasiNote5


class ClawQueryClient:
    """Claw 查询客户端"""

    def __init__(self, stream: bool = True):
        self.ws = None
        self.request_id = 0
        self.stream = stream
        self.pending_requests = {}  # id -> future
        self.messages = []

    def next_id(self) -> int:
        self.request_id += 1
        return self.request_id

    async def connect(
        self,
        token: str,
        tenant_id: str,
        app_key: str,
        use_uc: bool = False,
        app_id: str = "",
        app_secret: str = "",
    ):
        """建立连接并认证

        Args:
            token: 用户认证 token（JWT 或 UC token）
            tenant_id: 租户ID
            app_key: App Key
            use_uc: 是否使用 UC token 认证
            app_id: App 接入 ID（UC 模式需要）
            app_secret: App 接入 Secret（UC 模式需要）
        """
        headers = {"Authorization": f"Bearer {token}"}
        if tenant_id:
            headers["X-Tenant-ID"] = tenant_id

        # UC token 模式：需要设置 UserType 和 App 接入 headers
        if use_uc:
            headers["UserType"] = "UC"
            if app_id:
                headers["AppId"] = app_id
            if app_key:
                headers["AppKey"] = app_key
            if app_secret:
                headers["AppSecret"] = app_secret
        else:
            # JWT 模式：使用 X-App-Key
            if app_key:
                headers["X-App-Key"] = app_key

        self.ws = await websockets.connect(
            WS_URL,
            additional_headers=headers,
            ping_interval=30,
            ping_timeout=10
        )

        # 启动消息接收循环
        asyncio.create_task(self._receive_loop())

        # 1. initialize
        await self._send_request("initialize", {
            "protocolVersion": 1,
            "clientInfo": {"name": "claw", "version": "1.0.0"}
        })

        # 2. authenticated
        auth_params = {"accessToken": token}
        if tenant_id:
            auth_params["tenantId"] = tenant_id

        # UC 模式：在 params 中传递 appId、appKey、appSecret、userType
        if use_uc:
            auth_params["userType"] = "UC"
            if app_id:
                auth_params["appId"] = app_id
            if app_key:
                auth_params["appKey"] = app_key
            if app_secret:
                auth_params["appSecret"] = app_secret
        else:
            # JWT 模式：使用 wsAppKey
            if app_key:
                auth_params["appKey"] = app_key

        auth_resp = await self._send_request("authenticated", auth_params)

        # 检查 authenticated 是否成功
        if "error" in auth_resp:
            error_msg = auth_resp.get("error", {}).get("message", "认证失败")
            raise ValueError(f"认证失败: {error_msg}")

        if self.stream:
            print("[已连接]", flush=True)

    async def _send_request(self, method: str, params: dict) -> dict:
        """发送请求并等待响应"""
        req_id = self.next_id()
        future = asyncio.get_event_loop().create_future()
        self.pending_requests[req_id] = future

        await self.ws.send(json.dumps({
            "jsonrpc": "2.0",
            "id": req_id,
            "method": method,
            "params": params
        }))

        return await future

    async def _receive_loop(self):
        """接收消息循环"""
        try:
            async for msg in self.ws:
                data = json.loads(msg)

                # 响应消息
                if "id" in data and data["id"] is not None:
                    req_id = data["id"]
                    if req_id in self.pending_requests:
                        future = self.pending_requests.pop(req_id)
                        if not future.done():
                            future.set_result(data)

                # 通知消息
                elif data.get("method") == "session/update":
                    update = data.get("params", {}).get("update", {})
                    update_type = update.get("type") or update.get("sessionUpdate", "")

                    if update_type == "agent_message_chunk":
                        content = update.get("content", {})
                        text = content.get("text", "") if isinstance(content, dict) else ""
                        if text:
                            self.messages.append(text)
                            if self.stream:
                                print(text, end="", flush=True)

        except Exception as e:
            # 连接关闭或出错
            pass

    async def create_session(self) -> tuple[str, dict]:
        """创建会话，返回 (sessionId, 响应详情)"""
        resp = await self._send_request("session/new", {"cwd": "/app"})
        session_id = resp.get("result", {}).get("sessionId")
        return session_id, resp

    async def query(self, session_id: str, question: str) -> dict:
        """执行查询"""
        self.messages = []

        # 发送 prompt 并等待响应（响应包含 stopReason）
        resp = await self._send_request("session/prompt", {
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": question}]
        })

        result = resp.get("result", {})
        stop_reason = result.get("stopReason")

        if self.stream:
            print()  # 换行

        return {
            "success": True,
            "answer": "".join(self.messages),
            "stop_reason": stop_reason
        }

    async def close(self):
        """关闭连接"""
        if self.ws:
            await self.ws.close()


async def query(
    question: str,
    token: str,
    tenant_id: str = "",
    app_key: str = "",
    stream: bool = True,
    use_uc: bool = False,
    app_id: str = "",
    app_secret: str = "",
) -> dict:
    """执行查询

    Args:
        question: 用户问题
        token: 用户认证 token（JWT 或 UC token）
        tenant_id: 租户ID
        app_key: App Key
        stream: 是否流式输出
        use_uc: 是否使用 UC token 认证
        app_id: App 接入 ID（UC 模式需要）
        app_secret: App 接入 Secret（UC 模式需要）
    """
    client = ClawQueryClient(stream=stream)
    try:
        await client.connect(
            token,
            tenant_id or TENANT_ID,
            app_key or APP_KEY,
            use_uc=use_uc,
            app_id=app_id,
            app_secret=app_secret,
        )
        session_id, session_resp = await client.create_session()
        if not session_id:
            return {"success": False, "error": "创建会话失败", "session_response": session_resp}
        return await client.query(session_id, question)
    except Exception as e:
        return {"success": False, "error": str(e)}
    finally:
        await client.close()


def main():
    parser = argparse.ArgumentParser(description="Claw 数据查询工具")
    parser.add_argument("--token", required=True, help="UC token")
    parser.add_argument("--question", required=True, help="用户问题")
    parser.add_argument("--tenant", default=TENANT_ID, help="租户ID")
    parser.add_argument("--no-stream", action="store_true", help="禁用流式输出")

    args = parser.parse_args()

    result = asyncio.run(query(
        args.question,
        args.token,
        args.tenant,
        APP_KEY,  # 固定使用 EasiNote5
        stream=not args.no_stream,
        use_uc=True,  # 默认使用 UC 模式
        app_id="",
        app_secret="",
    ))

    if not args.no_stream:
        if not result.get("success"):
            print(f"\n错误: {result.get('error')}")
    else:
        print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()