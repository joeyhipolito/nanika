# rpc — Unix-socket JSON-RPC server (Obsidian plugin)

Newline-delimited JSON over a Unix domain socket. One request at a time per
connection; 5-second per-request deadline; 1 MiB line cap; socket at 0700.

## Wire format

```
→ {"method":"Ping|IndexStat|Recall","params":{...}}\n
← {"ok":true,"result":{...}}\n
← {"ok":false,"error":{"code":N,"message":"..."}}\n
```

## Methods

| Method      | Params                                    | Result                              |
|-------------|-------------------------------------------|-------------------------------------|
| `Ping`      | `{}`                                      | `{}`                                |
| `IndexStat` | `{}`                                      | `note_count`, `vertex_count`, `edge_count` |
| `Recall`    | `{"seed":"…","max_hops":2,"limit":20}`    | `{"paths":["a.md","b.md"]}`         |

## Usage

```go
// Server
srv := rpc.New(rpc.Config{Store: store, Graph: g})
srv.Start("/tmp/obs.sock")
defer srv.Shutdown(ctx)

// Client
c, _ := rpc.Dial("/tmp/obs.sock")
defer c.Close()
c.Ping(ctx)
stat, _ := c.IndexStat(ctx)
resp, _ := c.Recall(ctx, rpc.RecallRequest{Seed:"notes/index.md", MaxHops:2, Limit:20})
```

## Error codes

`1` ErrCodeBadRequest · `2` ErrCodeUnknownMethod · `3` ErrCodeInternal
