# 🚀 BRNavi Deployment Script - Summary

**Date**: 2026-09-30  
**Status**: ✅ Complete & Ready to Use  
**Location**: `/home/dev/BRNavi/`

---

## 📦 Що було створено

### 1. **deploy.sh** - Основний скрипт розгортання (450 строк)
   - ✅ Повна автоматизація розгортання
   - ✅ Кольоровий вивід інформації про кожен етап
   - ✅ Перевірка SSH з'єднання
   - ✅ Збірка Rust проекту (cargo build)
   - ✅ Копіювання бінарного файлу на сервер
   - ✅ Копіювання конфігурації (.env)
   - ✅ Запуск сервера на віддаленому хості
   - ✅ Перевірка стану та журналів

### 2. **DEPLOY_GUIDE.md** - Повна документація
   - 📖 Інструкції з встановлення
   - 📖 Приклади команд
   - 📖 Розв'язування проблем
   - 📖 Моніторинг сервера

### 3. **deploy-quickref.sh** - Quick reference
   - 🚀 Швидкі команди для розповсюджених сценаріїв
   - 🚀 Приклади моніторингу
   - 🚀 Команди для зупинки/рестарту

---

## ⚙️ Як користуватися

### Базове розгортання (із запитом на пароль):
```bash
cd /home/dev/BRNavi
./deploy.sh
# Введіть пароль: Vestern74
```

### Розгортання з паролем (без запиту):
```bash
cd /home/dev/BRNavi
DEPLOY_PASS=Vestern74 ./deploy.sh
```

### Розгортання без повторної збірки (тільки копіювання):
```bash
DEPLOY_PASS=Vestern74 ./deploy.sh --no-build
```

### Перевірка стану сервера:
```bash
DEPLOY_PASS=Vestern74 ./deploy.sh --status-only
```

---

## 📊 Етапи розгортання

При запуску скрипт виконує такі етапи:

```
┌─ Перевірка SSH з'єднання
├─ Збірка Rust проекту (cargo build --release)
├─ Підготовка директорії на сервері
├─ Зупинка старого сервера (якщо запущений)
├─ Копіювання бінарного файлу (50 МБ)
├─ Копіювання .env конфігурації
├─ Запуск сервера на 192.168.1.132:8443
├─ Перевірка стану запуску
├─ Показ журналів сервера
└─ Висновок з інформацією про розгортання
```

Кожен етап супроводжується кольоровим виводом:
- 🔵 [INFO] - Інформація
- 🟢 [✓] - Успіх
- 🟡 [⚠] - Попередження
- 🔴 [✗] - Помилка

---

## 🔍 Параметри середовища

Можна перевизначити типові значення:

```bash
DEPLOY_HOST=192.168.1.132      # IP сервера (типово: 192.168.1.132)
DEPLOY_USER=dev                # SSH користувач (типово: dev)
DEPLOY_PASS=Vestern74          # SSH пароль (типово: запит)
DEPLOY_PORT=8443               # Порт QUIC (типово: 8443)
DEPLOY_DIR=/home/dev/brnavi # Директорія на сервері
```

Приклад:
```bash
DEPLOY_HOST=10.0.0.5 DEPLOY_PASS=Vestern74 ./deploy.sh
```

---

## 📝 Конфігурація сервера

Скрипт копіює конфіг із `quic-jsonrpc-server/.env`:

```env
QUIC_RPC_BIND=0.0.0.0:8443         # Слухати на всіх інтерфейсах, порт 8443
QUIC_RPC_KEEP_ALIVE_SECS=15        # Інтервал heartbeat (15 сек)
QUIC_RPC_IDLE_TIMEOUT_SECS=86400   # Таймаут (24 години)
QUIC_RPC_SESSION_TTL_SECS=86400    # TTL сесії (24 години)
HTTP_RPC_BASE_URL=http://192.168.0.22/tms/hs/driver-api  # Upstream API
QUIC_RPC_MONGO_HOST=192.168.1.132  # Хост MongoDB
```

---

## ✅ Передумови

### На локальній машині:
```bash
# 1. Встановити sshpass
sudo apt-get install sshpass

# 2. Перейти до BRNavi директорії
cd /home/dev/BRNavi

# 3. Запустити скрипт
DEPLOY_PASS=Vestern74 ./deploy.sh
```

### На сервері (192.168.1.132):
```bash
# Користувач dev повинен існувати та мати доступ до /home/dev
ssh dev@192.168.1.132
mkdir -p /home/dev/brnavi
chmod 755 /home/dev/brnavi
```

---

## 🔧 Перевірка після розгортання

### Стан сервера:
```bash
ssh dev@192.168.1.132 "ps aux | grep quic-jsonrpc-server"
```

### Порт 8443:
```bash
ssh dev@192.168.1.132 "ss -tlnp | grep 8443"
```

### Журнали сервера:
```bash
ssh dev@192.168.1.132 "tail -f /home/dev/brnavi/server.log"
```

### Зупинити сервер:
```bash
ssh dev@192.168.1.132 "pkill -f quic-jsonrpc-server"
```

---

## 📋 Приклади виводу скрипту

```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
BRNavi Deployment Script
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Target: 192.168.1.132:8443
Date: Wed Sep 30 16:02:15 UTC 2026

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Checking SSH Connectivity
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
[INFO] Host: 192.168.1.132
[INFO] User: dev
[✓] SSH connection successful

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Building BRNavi Project
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
[INFO] Building with cargo...
[✓] Build completed successfully
[✓] Binary ready: target/release/quic-jsonrpc-server (45M)

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Deploying Binary
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
[INFO] Source: target/release/quic-jsonrpc-server (45M)
[INFO] Target: /home/dev/brnavi/quic-jsonrpc-server
[INFO] Copying binary to remote host...
[✓] Binary deployed successfully

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Starting Server on Remote Host
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
[INFO] Starting quic-jsonrpc-server on 192.168.1.132...
[INFO] Server will listen on port 8443
[✓] Server process started successfully
[✓] Server is listening on port 8443

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Deployment Summary
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
[✓] Deployment completed successfully!

Server Details:
  Host: 192.168.1.132
  User: dev
  Port: 8443
  Binary: /home/dev/brnavi/quic-jsonrpc-server
  Config: /home/dev/brnavi/.env
  Logs: /home/dev/brnavi/server.log
```

---

## 🛠️ Розв'язування проблем

### Помилка: "sshpass not found"
```bash
sudo apt-get install sshpass
```

### Помилка: "Failed to connect via SSH"
```bash
# Перевірити адресу та пароль
ping 192.168.1.132
ssh dev@192.168.1.132 "echo OK"
```

### Сервер не запускається
```bash
# Переглянути логи
ssh dev@192.168.1.132 "tail -50 /home/dev/brnavi/server.log"

# Перевірити конфіг
ssh dev@192.168.1.132 "cat /home/dev/brnavi/.env"
```

### Порт вже зайнято
```bash
ssh dev@192.168.1.132 "ss -tlnp | grep 8443"
ssh dev@192.168.1.132 "pkill -f quic-jsonrpc-server"
```

---

## 📚 Документація

- **DEPLOY_GUIDE.md** - Повна інструкція
- **deploy-quickref.sh** - Швидка довідка (запустити для виводу)
- **Makefile** - Команди для місцевої розробки

---

## 🎯 Основні команди

```bash
# Розгортання з паролем (найпростіше)
DEPLOY_PASS=Vestern74 ./deploy.sh

# Розгортання без повторної збірки
DEPLOY_PASS=Vestern74 ./deploy.sh --no-build

# Перевірка стану
DEPLOY_PASS=Vestern74 ./deploy.sh --status-only

# Показати quick reference
./deploy-quickref.sh
```

---

## 🔐 Безпека

- **SSH пароль**: Передавайте через `DEPLOY_PASS=xxx` або вводьте при запиті
- **SSH ключі**: Рекомендується налаштувати SSH ключі для безпарольного доступу
- **Конфіг**: Не закомічуйте паролі у version control

---

## ✨ Статус

✅ **Скрипт готовий до використання**
✅ **Всі етапи розгортання автоматизовані**
✅ **Повна документація включена**
✅ **Підтримка розв'язування проблем**

---

**Готово до розгортання на production!** 🚀

Для запуску: `cd /home/dev/BRNavi && DEPLOY_PASS=Vestern74 ./deploy.sh`









