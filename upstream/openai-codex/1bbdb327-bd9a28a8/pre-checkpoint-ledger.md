# Upstream Semantic Ledger

- Repository: `openai/codex`
- Branch: `main`
- Window: `pre_checkpoint`
- Range: `b89ce9a2bcedcfddf3a48f387b7912d602d6d87c..1bbdb32789e1f79932df44941236ea3658f6e965`
- Complete: no

## Coverage

| Measure | Commits |
|---|---:|
| Audit | 1017 |
| Classified | 6 |
| Missing | 1011 |
| Blocking | 4 |

## Classifications

| Subject | Area | Mechanical / Path | Disposition | Implementation | Summary | Evidence |
|---|---|---|---|---|---|---|
| `8f1aad58dc3a684afecdd9efd31d074762da3c24` | `other_codex_rs` | `8f1aad58dc3a:missing_patch/other_codex_rs` | `adopted` | `implemented` | Ignore RUSTSEC-2026-0173 for proc-macro-error2 in both cargo-audit and cargo-deny through local commit 83c14c3676, matching upstream's temporary suppression intent. | github_issue:cbusillo/codex-lab#418, github_pull_request:openai/codex#26974, git_commit:openai/codex@8f1aad58dc3a684afecdd9efd31d074762da3c24, git_commit:cbusillo/codex-lab@83c14c3676b6ecae9d717b3a869e806087685b93 |
| `9e0d7f02c9416c46dde6e571068a0fb03a4facdf` | `app_server` | `9e0d7f02c941:patch_equivalent/app_server` | `adopted` | `implemented` | Adopted through patch-equivalent local commit 15f0bf854b, preserving canonical auto_review serialization, legacy guardian_subagent input, and delegated reviewer propagation. | github_issue:cbusillo/codex-lab#407, github_pull_request:openai/codex#26230, git_commit:openai/codex@9e0d7f02c9416c46dde6e571068a0fb03a4facdf, git_commit:cbusillo/codex-lab@15f0bf854bf82de2eaf3524f089dc8efaa88a307 |
| `ee6c91d5cfd0e63239c75b41f4a2dc14130d5688` | `other_codex_rs` | `ee6c91d5cfd0:missing_patch/other_codex_rs` | `missing` | `missing` | Stop emitting the free-form codex_error_subreason analytics field copied from InvalidRequest text; the bounded 512-byte fork copy still carries provider or user-derived content. | github_issue:cbusillo/codex-lab#418, github_pull_request:openai/codex#27060, git_commit:openai/codex@ee6c91d5cfd0e63239c75b41f4a2dc14130d5688, git_commit:cbusillo/codex-lab@7fde0b24d31cdf5d1270d288e37c7130cae8efc2 |
| `daf76a57d2564be85b6e34c25a29380b3d4315b4` | `other_codex_rs` | `daf76a57d256:missing_patch/other_codex_rs` | `missing` | `missing` | Prune stale curated plugin cache entries when their names disappear from the raw marketplace while preserving user configuration. | github_issue:cbusillo/codex-lab#418, github_pull_request:openai/codex#26934, git_commit:openai/codex@daf76a57d2564be85b6e34c25a29380b3d4315b4, git_commit:cbusillo/codex-lab@7fde0b24d31cdf5d1270d288e37c7130cae8efc2 |
| `381f0de531e0bc7759863295fc333dd0087b4faf` | `other_codex_rs` | `381f0de531e0:missing_patch/other_codex_rs` | `missing` | `missing` | Serve plugin/list from the cached global remote catalog when present and refresh it asynchronously without duplicating cache-miss fetches. | github_issue:cbusillo/codex-lab#418, github_pull_request:openai/codex#26932, git_commit:openai/codex@381f0de531e0bc7759863295fc333dd0087b4faf, git_commit:cbusillo/codex-lab@7fde0b24d31cdf5d1270d288e37c7130cae8efc2 |
| `51b3cd51f6f94488c0e05564cbcad9512f73e3db` | `other_codex_rs` | `51b3cd51f6f9:missing_patch/other_codex_rs` | `missing` | `missing` | Restrict list_available_server_infos to codex-mcp and make new_uninitialized test-only without importing method-order churn or hiding the production cross-crate permission-profile constructor. | github_issue:cbusillo/codex-lab#418, github_pull_request:openai/codex#27257, git_commit:openai/codex@51b3cd51f6f94488c0e05564cbcad9512f73e3db, git_commit:cbusillo/codex-lab@7fde0b24d31cdf5d1270d288e37c7130cae8efc2 |

## Missing Commits

- `2ee3358c00a4d75db319d011013754a452ddddad`
- `e6c470957de53572706afd19f7d8efb42985864c`
- `e093d819826127be01354e2885c86d7fcbfb2897`
- `e648ec771f1f130e82110c9b30932361dcefe85f`
- `5a440c03f2f3393169c5df517d1fd8eee969e45e`
- `ed6e5cf919fdb8b388eb5643669dc175f26188cb`
- `4e803a017c958dd37eb251372ba810232d3e84ba`
- `743f5aad38accd52da34bf4dcbdd1215a8c3ab9a`
- `8d415050fce4b4ebc6da1ba247379844235fa453`
- `0526cb56ac3501a02968010d03873993c319e290`
- `26d932983398147e4443bd655ce24a6ce6833a1c`
- `6d0e313e237b6c1dd055fc9b1d7469961e8c02f8`
- `b128da272e640371c75913033bdcc96bcea85ec4`
- `a81531146609a25847ae8d045f30234e6c7eafd7`
- `f1c18df9aeacb10bb88f56ff6725be252791c705`
- `6d8e12ac42508c10bbe1d1769aee7f49d150ba5c`
- `0aa9931aeadd9cbb1d6d02f854d1402f8db2bec8`
- `2375cb64493367c3f8ff3cb98fcff2b6350ac678`
- `f3a807497590a8b86ef44480606f92cc71cf52f1`
- `f9a680b9075562093ff78e45ff4fcb2e9a0348f9`
- `f3c1283411edadcc0522bea376d0adc6961d5ca3`
- `e0ee491df351ad85d35f879d2e2b8b30c866814f`
- `85fd52f7e4aebfc39229b50e6f0c092dd00ca343`
- `123cf62a485de23a8960260f5bb00b86af6778c5`
- `56554904babcaacf4444a2cc90716880837dff7c`
- `b89d91f6ffee5474f23a97b23b401644c4e087d5`
- `feca160da47b678b73b33dd8a08e010e86b81786`
- `0473a5cc522cd8d0a798595c0a08ce661eb2a0ab`
- `6042e5810e7062cc937bb0397afaa5bea431fbc4`
- `ffec7c093365eacb2b5ef58dafd53abaeea72e03`
- `4ca2e436e50951baa1ea74246fe09bbbff1fda4a`
- `8534912df930700788ea69812de65e7222eaea53`
- `e127a0cf99195e94e8762c40c6ba17fd6158ac29`
- `c656cc4a83eaeb1289c8c8c66ea72b35dbc6e28b`
- `0beb5c7f32cf5459a51e3f6bc01e6509d7951854`
- `08cb633c06a27d25872d0132fbd9c749556d7653`
- `dffc4bf75dc9eeb0727f668c95bb7bd72315581d`
- `14660c22d14312c28a50c52954dd77dd88f03c26`
- `a304569c796a0aceeb9221e4bd8daba0102d39a0`
- `fae270932065355b5d7f197b3f1c72912588369b`
- `1547785657607043309fbe19d826aea8ee6e40e0`
- `a770e5b8470d3320eb53a56a286ea4a0a70a1f59`
- `18ce671fed526be9033907bd88a3a63c6888bbf4`
- `6a9a49b334e8081756934b0ce7d909234b53aac7`
- `8a299fb7043b645cecb50b239beb7d0e9e4ac7e7`
- `1026e9de1be292ebb01579dfcce5a34ab224917a`
- `89ac3ec27cae0000da14fbe4160a79ed465843fb`
- `99da697e4c5c1fc908732a58b6548bf9cc227f83`
- `a71e040df51ec0e1f5523bf0ff5f1bef39858128`
- `6e7ab529297ac3e8ef9110718e1eab688d4391ef`
- `7a7cee1be4e82733f393c95180242d11b50064d5`
- `472619f9fc6739d5c4fa499b3ef10007655b031d`
- `36bf63a5cf02dafb1574b2396597ed1ef16bb5d7`
- `8e69d29521488506182d2b5814851cdbb90c1354`
- `5a0f9134267e0fed2406fce0ea0f14e30894513e`
- `4ec3b8eeea38e9f693568e4f51bb607cc6cd9717`
- `9e3081be9672c65f8a0cd958719065f49f47d839`
- `f574946960a4d21e884453ac9963a869153b847d`
- `f2969f36e8b6aa2aefcd625d2a9fc8425bb2a519`
- `cc8325f181401c481f4da8ca16c073867e78d46e`
- `9316acf9b238da7559d7fba67c612712acc3b419`
- `490340ffcf593518bcd843ee903d1ea2160d5363`
- `fb8f1ea0d55bb0a6202961518440b40db37b3802`
- `51fc4b0559c08d12caec6f4d7d2b6a35e84c7339`
- `5ac640ac49ae5c8b781d51bbaf0467c98cda2643`
- `4a3eac214494a5cefebe53308dc48ebabf03201c`
- `608b8b1cc6ce91064e1fd12e0810e1772b5e4710`
- `e0cb4ede4e44a371d595520b29d0c80336b8733e`
- `00a25e1e0c6eecf076dcb989f4065c578c262ae7`
- `a7b6baecc63519fd1d7b7be4f779217fe6c82063`
- `0ffcefaf3ddb3a61d8683ca0703f7d8b39ad6c1e`
- `9cd11e9e62c48ea6127f8b9f6ae2e013e4e4ff00`
- `ced1b8aa883f25b992f9ebe7d218f3709926f912`
- `d2f6d23c6cb07f72d18dfd0d8ca09c807870e677`
- `41b4fabbb4fb2a5d9d956d12c706daee992a76e2`
- `30ddb3325e380d8e32d39324bd9acf931e5217bb`
- `0428e20a0b8a1153453832c616dc96133aa4b127`
- `24aee3eb5ee90dd211ef183d65fec774dc7049c9`
- `c365b8a4abfe0436ca85978fd186bd89d55a8807`
- `db531b4a6c07dd8a4cb624f25b8d6abbd28ccf96`
- `2ef007dc1ad60f772a3e0ac6560d61f783783b9d`
- `d3abd8774ed2caa13db253ee232791e5a2d2d8c5`
- `2704ecea9a1d52ece2429da4ed5775000b59164d`
- `a1a8807e9d67fad4b95f2730a9669eca5a9d27d0`
- `a19d43a40aee5f3308ce57eda604fa085fd9f356`
- `3691fe5b76ccfa84835936c63efffb053dc8e6c6`
- `636cc11398b805a9ad89a4ce2b45ded2feb59567`
- `42415443d036c374eb848caec69b5714e9681b35`
- `ccf1a185180428727cffbdd9bd4eaaab2dc218ef`
- `72667f4f41ec5515096ea5676b09cd3c01e6c866`
- `020bf49346efa6c781726c86eda2a59ff415a712`
- `2e377ce5e5bc3da1ca2b133496cbccab3a3d2c01`
- `13468115fc6443970b8bf521927fceaf58ad35c1`
- `e3528434cdfb9b822057095c7edbf4c99363f709`
- `1deae7bd4a1212f94ec5b877fc4c9d7edea644f1`
- `b4445f275838981bfd22429c5c41f2c5be0bde7a`
- `7011044c1c0f51185eee007b45f0f42140fa794c`
- `980f60b6641c5907c16db3c39f36ac113e15c93d`
- `387adc6c4bc484aefb658a6006ad0a26fc45c79b`
- `22dd6ebc7d3abe50d8aa40be3025eccd9c166418`

_911 additional commits omitted; run `validate` for the complete machine-readable list._

## Blocking Commits

- `ee6c91d5cfd0e63239c75b41f4a2dc14130d5688`
- `daf76a57d2564be85b6e34c25a29380b3d4315b4`
- `381f0de531e0bc7759863295fc333dd0087b4faf`
- `51b3cd51f6f94488c0e05564cbcad9512f73e3db`
