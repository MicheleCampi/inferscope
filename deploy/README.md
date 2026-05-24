# Deployment Examples

This directory contains example deployment manifests for running `inferscope` in environments other than a developer shell. It is **example material, not production configuration**: the files are valid and runnable starting points, but every production deployment needs choices that depend on the operator's specific cluster, image registry, secret management, and observability stack.

## What's here

| File | Use case |
|---|---|
| [`docker-compose.yml`](docker-compose.yml) | Local runs against a locally hosted engine, with the inferscope image built from the repo's Dockerfile and GPU access via NVIDIA Container Toolkit. |
| [`inferscope-job.yaml`](inferscope-job.yaml) | Kubernetes Job manifest for a one-shot profiling run on a GPU-equipped cluster with the NVIDIA Device Plugin installed. |

## Quick start

### Docker Compose (local)

```bash
docker compose -f deploy/docker-compose.yml up --build
```

Edit the `command:` block in `docker-compose.yml` to point at your actual engine endpoint and model. The container will run inferscope once and exit. The plain-text report appears in the container logs; if `--json` is used the JSON lands in `./output/` (host-side bind mount).

Requires NVIDIA Container Toolkit on the host. If the run fails with "GPU sampling requested but unavailable", see [`RUNBOOK.md`](../RUNBOOK.md) Scenario 7.

### Kubernetes Job (cluster)

```bash
# Edit inferscope-job.yaml to point at your engine service and model
vim deploy/inferscope-job.yaml

# Apply
kubectl apply -f deploy/inferscope-job.yaml

# Watch
kubectl logs -f job/inferscope-run

# Clean up after inspection
kubectl delete job inferscope-run
```

Requires a cluster with NVIDIA Device Plugin (`nvidia.com/gpu` resource available) and a published `inferscope` image accessible from the cluster. The manifest defaults to `ghcr.io/michelecampi/inferscope:v0.2.1` — change this to your registry if you mirror the image.

## Design choices and trade-offs

A few decisions in these manifests are intentional and worth understanding before adapting them.

**Job, not Deployment.** `inferscope` runs to completion and exits. A `Deployment` would restart it forever, which is wrong for a profiler. `Job` is the right primitive: run once, record success or failure, leave the pod around for `kubectl logs` until cleaned up.

**`backoffLimit: 0`.** A failed profiling run is a signal, not a thing to retry. Repeated runs against the same engine in quick succession confound timing measurements (cache warmup, lazy CUDA graph capture — see [the article on vLLM cold-start tiers](../#)). If the Job fails, investigate the failure manually rather than letting Kubernetes mask it with retries.

**`emptyDir` for output.** The default volume is `emptyDir`, which loses the JSON when the pod terminates. For local exploration this is fine — `kubectl logs` captures the human-readable report. For workflows that consume the JSON downstream, replace `emptyDir` with a PersistentVolumeClaim or an `emptyDir` + sidecar that uploads to object storage before pod termination.

**No nodeSelector by default.** Production clusters typically taint GPU nodes; the operator's existing tolerations/selectors should apply. The example includes a commented `nodeSelector` line showing the pattern for targeting a specific GPU type (H100, A100, L4).

**Single image tag, not `latest`.** The manifest pins `v0.2.1` rather than `latest`. Pinning protects against silent upgrades changing the profiler's behaviour mid-experiment.

## What this directory does NOT cover

- **CronJob for scheduled profiling.** If you want to profile your engine every night at 03:00 UTC, wrap the `Job` template in a `CronJob`. Not provided here because the schedule belongs to the operator's runbook, not to inferscope.
- **Sidecar pattern.** Running inferscope alongside the engine in the same Pod (sharing PID namespace, so `--pid` resolves to the engine container) is a valid pattern for continuous observability. Not provided here because it requires careful image / namespace configuration that depends on the specific engine.
- **Helm chart / Kustomize overlays.** This directory contains raw manifests. If your cluster uses Helm or Kustomize, treating these files as the source-of-truth template and parameterising endpoint/model/image via your tool of choice is the recommended adaptation.
- **Image signing / supply chain attestation.** The published image is unsigned at present. See [`SECURITY.md`](../SECURITY.md) "Known Limitations" point 3.

## Building the image yourself

If you don't want to pull from `ghcr.io/michelecampi/inferscope`, the [`Dockerfile`](../Dockerfile) at the repository root is the multi-stage source. Build it with:

```bash
docker build -t inferscope:custom .
```

Then either push to your own registry or load it into your cluster's local store (kind, minikube). Substitute the `image:` field in `inferscope-job.yaml` accordingly.

## Reporting issues

Bugs in these manifests (typos, invalid syntax, missing fields) should be reported as regular GitHub issues. Security issues — for example, if the manifest exposes credentials or grants excessive permissions — should follow [`SECURITY.md`](../SECURITY.md).
