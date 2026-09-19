# rubocop:disable Metrics/AbcSize
def foo
end

def bar
end
# rubocop:enable Metrics/AbcSize
foo # rubocop:disable Metrics/AbcSize
# rubocop:disable-next Metrics/MethodLength
def baz
end
