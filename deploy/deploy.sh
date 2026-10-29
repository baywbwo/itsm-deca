#!/usr/bin/env bash
set -euo pipefail
sudo mkdir -p /opt/zammad && sudo chown vobiz:vobiz /opt/zammad && cd /opt/zammad
wget -q -O docker-compose.yml https://raw.githubusercontent.com/zammad/zammad-docker-compose/master/docker-compose.yml
wget -q -O .env.dist https://raw.githubusercontent.com/zammad/zammad-docker-compose/master/.env.dist
PG=$(openssl rand -hex 16)
sed -e "s|^POSTGRES_PASS=.*|POSTGRES_PASS=$PG|" \
    -e "s|^NGINX_EXPOSE_PORT=.*|NGINX_EXPOSE_PORT=127.0.0.1:8080|" \
    -e "s|^NGINX_PORT=.*|NGINX_PORT=8080|" \
    -e "s|^TZ=.*|TZ=Asia/Jakarta|" .env.dist > .env
grep -q '^POSTGRES_PASS=' .env || echo "POSTGRES_PASS=$PG" >> .env
grep -q '^NGINX_EXPOSE_PORT=' .env || echo "NGINX_EXPOSE_PORT=127.0.0.1:8080" >> .env
grep -q '^TZ=' .env || echo "TZ=Asia/Jakarta" >> .env
# password postgres harus terbaca service postgres
python3 - <<'PY'
p="/opt/zammad/docker-compose.yml"
s=open(p).read()
s=s.replace("      - POSTGRES_PASSWORD=\n","      - POSTGRES_PASSWORD=${POSTGRES_PASS}\n")
s=s.replace("      - POSTGRES_PASSWORD=\r\n","      - POSTGRES_PASSWORD=${POSTGRES_PASS}\n")
open(p,"w").write(s)
PY
grep -n "POSTGRES_PASSWORD" docker-compose.yml | head -3
for i in 1 2 3 4 5; do
  echo "pull percobaan $i"
  sudo docker compose pull --quiet && break || sleep 8
done
sudo docker compose up -d
sleep 45
sudo docker compose ps --format "{{.Service}} {{.State}}"
