#!/usr/bin/env bash

################################################################################
#                    BRNavi Deploy - Quick Reference                        #
#                                                                              #
# One-liner examples for common deployment scenarios                          #
################################################################################

# Colors for documentation
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

echo -e "${BLUE}╔════════════════════════════════════════════════════════════════╗${NC}"
echo -e "${BLUE}║           BRNavi Deployment - Quick Reference              ║${NC}"
echo -e "${BLUE}╚════════════════════════════════════════════════════════════════╝${NC}"
echo ""

echo -e "${YELLOW}📝 QUICK DEPLOYMENT COMMANDS${NC}"
echo ""

echo -e "${GREEN}1. Full deployment (build + deploy):${NC}"
echo '   cd /home/dev/BRNavi'
echo '   ./deploy.sh'
echo '   # Then enter password: Vestern74'
echo ""

echo -e "${GREEN}2. Full deployment with password in command:${NC}"
echo '   cd /home/dev/BRNavi'
echo '   DEPLOY_PASS=Vestern74 ./deploy.sh'
echo ""

echo -e "${GREEN}3. Deploy without rebuild (faster for config changes):${NC}"
echo '   cd /home/dev/BRNavi'
echo '   DEPLOY_PASS=Vestern74 ./deploy.sh --no-build'
echo ""

echo -e "${GREEN}4. Check server status only:${NC}"
echo '   cd /home/dev/BRNavi'
echo '   DEPLOY_PASS=Vestern74 ./deploy.sh --status-only'
echo ""

echo -e "${GREEN}5. Deploy to different host:${NC}"
echo '   cd /home/dev/BRNavi'
echo '   DEPLOY_HOST=192.168.1.100 DEPLOY_PASS=Vestern74 ./deploy.sh'
echo ""

echo -e "${YELLOW}🔍 POST-DEPLOYMENT CHECKS${NC}"
echo ""

echo -e "${GREEN}1. Check if server is running:${NC}"
echo '   ssh dev@192.168.1.132 "ps aux | grep quic-jsonrpc-server"'
echo ""

echo -e "${GREEN}2. Check if port 8443 is listening:${NC}"
echo '   ssh dev@192.168.1.132 "ss -tlnp | grep 8443"'
echo ""

echo -e "${GREEN}3. View server logs (live):${NC}"
echo '   ssh dev@192.168.1.132 "tail -f /home/dev/brnavi/server.log"'
echo ""

echo -e "${GREEN}4. View last 50 lines of logs:${NC}"
echo '   ssh dev@192.168.1.132 "tail -50 /home/dev/brnavi/server.log"'
echo ""

echo -e "${YELLOW}🛑 STOP/RESTART SERVER${NC}"
echo ""

echo -e "${GREEN}1. Stop server:${NC}"
echo '   ssh dev@192.168.1.132 "pkill -f quic-jsonrpc-server"'
echo ""

echo -e "${GREEN}2. Start server manually:${NC}"
echo '   ssh dev@192.168.1.132 "cd /home/dev/brnavi && nohup ./quic-jsonrpc-server > server.log 2>&1 &"'
echo ""

echo -e "${GREEN}3. Restart server:${NC}"
echo '   ssh dev@192.168.1.132 "pkill -f quic-jsonrpc-server; sleep 2; cd /home/dev/brnavi && nohup ./quic-jsonrpc-server > server.log 2>&1 &"'
echo ""

echo -e "${YELLOW}🔐 SSH & SETUP${NC}"
echo ""

echo -e "${GREEN}1. Test SSH connection:${NC}"
echo '   ssh dev@192.168.1.132 "echo Connection OK"'
echo ""

echo -e "${GREEN}2. Create deploy directory on server:${NC}"
echo '   ssh dev@192.168.1.132 "mkdir -p /home/dev/brnavi && ls -ld /home/dev/brnavi"'
echo ""

echo -e "${GREEN}3. Copy SSH key for password-less login:${NC}"
echo '   ssh-copy-id -i ~/.ssh/id_rsa.pub dev@192.168.1.132'
echo ""

echo -e "${YELLOW}📊 MONITORING${NC}"
echo ""

echo -e "${GREEN}1. Real-time connection statistics:${NC}"
echo '   ssh dev@192.168.1.132 "watch -n 5 \"ss -tlnp | grep 8443\""'
echo ""

echo -e "${GREEN}2. Memory usage of server:${NC}"
echo '   ssh dev@192.168.1.132 "ps aux | grep quic-jsonrpc-server | awk '\''{print \$6}'\'' | tail -1"'
echo ""

echo -e "${GREEN}3. CPU usage of server:${NC}"
echo '   ssh dev@192.168.1.132 "top -b -n 1 | grep quic-jsonrpc-server"'
echo ""

echo -e "${YELLOW}🆘 TROUBLESHOOTING${NC}"
echo ""

echo -e "${RED}Problem: Port 8443 already in use${NC}"
echo '   ssh dev@192.168.1.132 "ss -tlnp | grep 8443"'
echo '   ssh dev@192.168.1.132 "pkill -f quic-jsonrpc-server"'
echo ""

echo -e "${RED}Problem: SSH password-less access needed${NC}"
echo '   ssh-keygen -t rsa -b 4096 -f ~/.ssh/id_rsa -N ""'
echo '   ssh-copy-id -i ~/.ssh/id_rsa.pub dev@192.168.1.132'
echo ""

echo -e "${RED}Problem: sshpass not installed${NC}"
echo '   sudo apt-get install sshpass          # Ubuntu/Debian'
echo '   brew install sshpass                  # macOS'
echo ""

echo -e "${YELLOW}⚙️ ENVIRONMENT VARIABLES${NC}"
echo ""
echo "Default values if not set:"
echo "  DEPLOY_HOST=192.168.1.132    (target server IP)"
echo "  DEPLOY_USER=dev              (SSH user)"
echo "  DEPLOY_PASS=<prompt>         (SSH password, will ask if not set)"
echo "  DEPLOY_PORT=8443             (QUIC server port)"
echo "  DEPLOY_DIR=/home/dev/brnavi (deploy directory on server)"
echo ""

echo -e "${YELLOW}📝 CONFIGURATION (.env)${NC}"
echo ""
echo "Key settings in /home/dev/BRNavi/quic-jsonrpc-server/.env:"
echo "  QUIC_RPC_BIND=0.0.0.0:8443   (listen on all interfaces, port 8443)"
echo "  QUIC_RPC_KEEP_ALIVE_SECS=15  (heartbeat interval)"
echo "  QUIC_RPC_IDLE_TIMEOUT_SECS=86400 (24 hour timeout)"
echo "  QUIC_RPC_MONGO_HOST=192.168.1.132 (MongoDB host)"
echo ""

echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo "For full documentation, see: /home/dev/BRNavi/DEPLOY_GUIDE.md"
echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"

