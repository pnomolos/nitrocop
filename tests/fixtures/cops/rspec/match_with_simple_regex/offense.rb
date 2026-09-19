expect('foobar').to match(/foo/)
                    ^^^^^^^^^^^^ RSpec/MatchWithSimpleRegex: Prefer using `include('foo')` when the regex is a simple string literal.
expect(response.body).to match(/http:\/\/example\.com/)
                         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ RSpec/MatchWithSimpleRegex: Prefer using `include('http://example.com')` when the regex is a simple string literal.
expect(response).to match(/it's "working"/)
                    ^^^^^^^^^^^^^^^^^^^^^^^ RSpec/MatchWithSimpleRegex: Prefer using `include("it's \"working\"")` when the regex is a simple string literal.
expect(response).to match(%r{a/b})
                    ^^^^^^^^^^^^^^ RSpec/MatchWithSimpleRegex: Prefer using `include('a/b')` when the regex is a simple string literal.
expect(response).to match(/closing ] and } are literal/)
                    ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ RSpec/MatchWithSimpleRegex: Prefer using `include('closing ] and } are literal')` when the regex is a simple string literal.
expect(response).to match(/a\-b\#c\@d/)
                    ^^^^^^^^^^^^^^^^^^^ RSpec/MatchWithSimpleRegex: Prefer using `include('a-b#c@d')` when the regex is a simple string literal.
expect(response).not_to match(/bar/)
                        ^^^^^^^^^^^^ RSpec/MatchWithSimpleRegex: Prefer using `include('bar')` when the regex is a simple string literal.
