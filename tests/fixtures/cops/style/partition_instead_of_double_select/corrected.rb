positives, negatives = arr.partition { |x| x > 0 }
separator_one
pos2, neg2 = arr.partition { |x| x > 0 }
separator_two
pos3, neg3 = arr.partition { |x| x > 0 }
separator_three
pos4, neg4 = arr.partition { |x| x > 0 }
separator_four
pos5, neg5 = arr.partition do |x|
  x > 0
end
separator_five
pos6, neg6 = arr.partition { _1 > 0 }
separator_six
pos7, neg7 = arr.partition { it > 0 }
separator_seven
pos8, neg8 = foo.bar.partition { |x| x > 0 }
separator_eight
pos9, neg9 = arr&.partition { |x| x > 0 }
separator_nine
arr.select { |x| x > 0 }
arr.reject { |x| x > 0 }
separator_ten
@positives = arr.select { |x| x > 0 }
@negatives = arr.reject { |x| x > 0 }
separator_eleven
pos10 = arr.select { |x| x > 0 }
@neg10 = arr.reject { |x| x > 0 }
separator_twelve
pos11, neg11 = arr.partition(&:positive?)
separator_thirteen
pos12, neg12 = arr.partition(&:positive?)
separator_fourteen
pos13, neg13 = arr&.partition(&:positive?)
separator_fifteen
pos14, neg14 = arr.partition(&:positive?)
separator_sixteen
pos15, neg15 = arr.partition { |x| x.positive? }
separator_seventeen
a1, b1 = arr.partition { |x| x.positive? }
separator_eighteen
a2, b2 = arr.partition { |x| x.positive? }
separator_nineteen
b3, a3 = arr.partition { |x| x.positive? }
separator_twenty
a4, b4 = arr.partition { _1.positive? }
