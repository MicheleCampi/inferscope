# Raw validation data

The archives behind the benchmark write-ups, extracted as recorded and not edited. Each
directory is the content of one tarball from the validation sessions; the SHA-256 is that
of the tarball. GPUs: L4 (20 May 2026), H100 80GB HBM3 (22 May), 4x A40 (23 May), H100
with vLLM (23-24 May). Hostnames are container IDs. The only addresses are loopback
(127.0.0.1) and a private Docker bridge address (172.20.0.2) in the vLLM server logs; no
public address, token or key, checked before publishing.

| Directory | Source archive | SHA-256 |
|---|---|---|
| [`l4-2026-05-20/validation-l4-2026-05-20/`](l4-2026-05-20/validation-l4-2026-05-20/) | `validation-l4-2026-05-20.tar.gz` | `62fda264a7ab48572e59a5e351836a9715bb3e65aea61c37b950476f13f9cfaa` |
| [`l4-2026-05-20/validation-l4-v0.2-empirical-2026-05-20/`](l4-2026-05-20/validation-l4-v0.2-empirical-2026-05-20/) | `validation-l4-v0.2-empirical-2026-05-20.tar.gz` | `0eb6319a0091c834bae0f56d035fff9d15882f8bf76ed6924a2b54a3d2baaef8` |
| [`h100-2026-05-22/`](h100-2026-05-22/) | `validation-h100-2026-05-22.tar.gz` | `1f506c4273cca9542ef19419c746af81db693accd44d3da61f467ca0455f35db` |
| [`multi-device-2026-05-23/`](multi-device-2026-05-23/) | `multigpu-validation-2026-05-23.tar.gz` | `4f6d83c5eb37a8b8441e3683cb5d6ac1d99b79f4e06118b2def50694a72ec569` |
| [`vllm-2026-05-24/`](vllm-2026-05-24/) | `vllm-validation-2026-05-24.tar.gz` | `1cd38b00d66cf4294021742d0232876e170160c91be5c75fa86a127d182e72fe` |

Used by [cross-hardware-comparison.md](../cross-hardware-comparison.md) (L4, H100, A40),
[multi-device-validation.md](../multi-device-validation.md) (A40) and
[vllm-vs-llama-cpp.md](../vllm-vs-llama-cpp.md) (vLLM).
