# nitrocop-config: AllowedClasses: [SpecificError, OtherError]
class Foo
end

class SpecificError
end

module OtherError
end

class Nested::SpecificError
end
