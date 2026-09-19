@ivar = File.open('file')

@@cvar = File.open('file')

$gvar = File.open('file')

CONST = File.open('file')

process(File.open('file'))

process(io: File.open('file'))

def json_key_io
  File.open('file')
end

File.open('file') { |f| f.read }

File.open('file') do |f|
  f.read
end

File.open('file', &:read)

File.open('file', &block)

File.read('file')

open('file')

Foo.open('file')

Foo::File.open('file')

File&.open('file')
