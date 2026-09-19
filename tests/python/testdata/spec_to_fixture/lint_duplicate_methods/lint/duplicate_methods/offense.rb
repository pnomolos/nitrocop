def something
end
def something
^^^^^^^^^^^^^ Lint/DuplicateMethods: Method `Object#something` is defined at both lib/foo.rb:1 and lib/foo.rb:3.
end

def something
end
def something
^^^^^^^^^^^^^ Lint/DuplicateMethods: Method `Object#something` is defined at both /no/project/root/foo.rb:1 and /no/project/root/foo.rb:3.
end
