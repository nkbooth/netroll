// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
// The config CI uses for a pull request GitHub records as opened by
// dependabot[bot], and for nothing else. Dependabot writes a compare URL longer
// than the body limit into every version update, which no one can rewrap
// before the check runs. Only that rule is lifted; no-story-refs and the
// conventional header still apply.
const base = require('./commitlint.config.cjs');

module.exports = { ...base, rules: { ...base.rules, 'body-max-line-length': [0] } };
