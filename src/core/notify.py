# -*- coding: utf-8 -*-
"""
推送。

关键改动：失败必须抛异常。
原脚本在 send() 内部 try/except 把所有异常吞掉，然后调用方照样写 time.txt，
结果是「推送没发出去，但被标记为已处理」，这条头目就永远丢了。
"""

import os

import requests

from .config import get_config


class NotifyError(RuntimeError):
    pass


def _post_wxpusher(content: str, summary: str) -> dict:
    cfg = get_config()
    wx = (cfg.notify.get("wxpusher") or {})
    if not wx.get("enabled", True):
        return {"skipped": True}

    token_env = wx.get("app_token_env", "WXPUSHER_APP_TOKEN_alpha")
    token = os.environ.get(token_env)
    if not token:
        raise NotifyError(f"环境变量 {token_env} 未设置，无法推送")

    payload = {
        "appToken": token,
        "content": content,
        "summary": summary or (wx.get("fallback_summary") or "头目"),
        "contentType": 1,
        "topicIds": wx.get("topic_ids", []),
    }
    try:
        resp = requests.post(
            "https://wxpusher.zjiecode.com/api/send/message",
            json=payload,
            timeout=20,
        )
        resp.raise_for_status()
        return resp.json()
    except Exception as e:
        raise NotifyError(f"WxPusher 推送失败: {e}")


def send(content: str, summary: str = None) -> None:
    """推送消息。失败抛 NotifyError，调用方据此决定是否写去重标记。"""
    cfg = get_config()
    on_failure = cfg.notify.get("on_failure", "raise")

    try:
        result = _post_wxpusher(content, summary)
    except NotifyError:
        if on_failure == "raise":
            raise
        return

    # WxPusher 用 HTTP 200 + body 里的 code 表示业务失败，必须一起判断
    if isinstance(result, dict) and not result.get("skipped"):
        if result.get("code") not in (1000, None):
            msg = f"WxPusher 返回业务错误: {result}"
            if on_failure == "raise":
                raise NotifyError(msg)
