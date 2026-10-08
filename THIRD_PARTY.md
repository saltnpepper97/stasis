# Third-party licensing

Stasis's own source remains licensed under GPL-3.0-only, as stated in `LICENSE`
and the source headers.

Automatic game discovery links `lib_game_detector` 0.0.39, by Rolv Apneseth,
which is licensed under AGPL-3.0-only. Its license text is included in
[`LICENSES/AGPL-3.0-only.txt`](LICENSES/AGPL-3.0-only.txt).

[GPLv3 section 13](https://www.gnu.org/licenses/gpl-3.0.html#section13) and
[AGPLv3 section 13](https://www.gnu.org/licenses/agpl-3.0.html#section13)
permit linking these works. The Stasis portion retains GPLv3 and the library
retains AGPLv3. AGPLv3's section 13 requirements concerning remote network
interaction apply to the combined work. Linking does not relicense the library
as GPL-only.

Distributors of the compiled program must account for both licenses, include
their notices and license texts, and satisfy the applicable corresponding-source
requirements, including the linked library. Package metadata describing the
compiled program should identify both `GPL-3.0-only` and `AGPL-3.0-only`.

Game discovery reads local launcher metadata; this feature adds no network
service and sends no library information to an external service.

Upstream: <https://github.com/Rolv-Apneseth/lib_game_detector>
