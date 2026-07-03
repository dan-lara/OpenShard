tar -czf /tmp/volunteer-src.tar.gz -C src/shard-node/container .
tar -tzf /tmp/volunteer-src.tar.gz | grep env   # confirm .env is in there
scp /tmp/volunteer-src.tar.gz root@100.81.196.38:/tmp/
