-- 双重验证的持久事实随管理员身份保存，密钥使用管理员密码派生的密钥加密
alter table admin_users add column mfa jsonb not null default '{"encrypted_secret":"","enabled":false,"setup_expires_at":0,"last_used_step":null,"recovery_hashes":[],"pending_recovery_hashes":[],"setup_id":""}';
alter table admin_users add constraint admin_users_mfa_object_ck check (jsonb_typeof(mfa) = 'object');
