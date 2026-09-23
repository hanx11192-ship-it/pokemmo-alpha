# -*- coding: utf-8 -*-
"""登录与会话：首登即管理员、密码哈希、登录态校验。"""
import functools

from werkzeug.security import generate_password_hash, check_password_hash
from flask import session, jsonify, redirect, request, url_for

import panel.db as db


def is_authenticated():
    return bool(session.get("user"))


def current_user():
    return session.get("user")


def current_role():
    return session.get("role")


def login_required(f):
    @functools.wraps(f)
    def wrapper(*args, **kwargs):
        if not is_authenticated():
            if request.path.startswith("/api/"):
                return jsonify(ok=False, error="未登录或会话已过期"), 401
            return redirect(url_for("login_page"))
        return f(*args, **kwargs)
    return wrapper


def do_login(username: str, password: str):
    """返回登录结果 dict，失败返回 None。
    首登（users 表为空）时，第一个登录者自动成为管理员。
    """
    username = (username or "").strip()
    if not username or not password:
        return None

    u = db.get_user(username)
    if u is None:
        if db.count_users() == 0:
            db.add_user(username, generate_password_hash(password), is_admin=True, lang="zh")
            session["user"] = username
            session["role"] = "admin"
            return {"username": username, "role": "admin", "is_first_admin": True}
        return None  # 用户不存在，且已有管理员

    if not check_password_hash(u["password_hash"], password):
        return None

    session["user"] = username
    session["role"] = "admin" if u["is_admin"] else "user"
    return {"username": username, "role": session["role"]}


def do_logout():
    session.clear()
