## You can test with my local llm:


```
cargo run --release -- \
  --p2p-port 8002 \
  --http-port 8890 \
  --bootstrap /ip4/172.232.46.58/tcp/1334
```

```
curl -X POST http://localhost:8890/v1/chat/completions \
  -H "Authorization: Bearer tutu" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "glm4", "stream": false,
    "messages": [
      {"role": "user", "content": "What is the capital of France?"}
    ]
  }'


//     you can change "stream": true, for stream respondes
  ```



---
my setup:
```
model: glm4
max_context: 5000
rate_limit:
  requests_per_minute: 20

```